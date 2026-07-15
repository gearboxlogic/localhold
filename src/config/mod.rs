//! Configuration loading from TOML files and environment variable overrides.

mod embedding;
pub mod operator;

use std::{
    collections::HashMap,
    fmt,
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use self::embedding::validate_openai_compatible_config;
pub use self::embedding::{DEFAULT_EMBEDDING_DIMENSIONS, EmbeddingConfig, EmbeddingHealthCheck, OpenAiAuthMode, OpenAiCompatibleConfig};
use self::embedding::{apply_embedding_env, normalize_embedding_config, validate_embedding_config};
use crate::{
    error::{EngineError, ParseEnumError},
    types::SearchMode,
};
/// Default `HuggingFace` model identifier for cross-encoder reranking.
///
/// The model card uses `L-6` (hyphenated), but the `HuggingFace` repo URL
/// uses `L6` (no hyphen). Both resolve to the same model; we accept both
/// forms via [`is_builtin_default_reranker_model`] to avoid user confusion.
pub const DEFAULT_RERANKER_MODEL: &str = "cross-encoder/ms-marco-MiniLM-L-6-v2";
/// Canonical upstream repo name for the default reranker model (unhyphenated `L6` form).
pub const DEFAULT_RERANKER_MODEL_CANONICAL: &str = "cross-encoder/ms-marco-MiniLM-L6-v2";
/// Pinned immutable revision for the default reranker model.
pub const DEFAULT_RERANKER_REVISION: &str = "c5ee24cb16019beea0893ab7796b1df96625c6b8";
/// Default HTTP header that carries a principal asserted by a trusted proxy.
pub const DEFAULT_HTTP_PRINCIPAL_HEADER: &str = "x-localhold-principal";
/// Default maximum HTTP request body size for streamable HTTP transport.
pub const DEFAULT_HTTP_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Default maximum number of simultaneously retained HTTP MCP sessions.
pub const DEFAULT_HTTP_MAX_SESSIONS: usize = 128;
/// Default idle lifetime for stateful HTTP MCP sessions.
pub const DEFAULT_HTTP_SESSION_IDLE_TIMEOUT_SECS: u64 = 900;
/// Maximum candidate-pool size supported by all current search backends.
pub const MAX_CANDIDATE_POOL_SIZE_CEILING: usize = 1000;

/// Requested ONNX Runtime execution provider for reranking.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RerankerExecutionProvider {
    /// Prefer CUDA when it is compiled and usable, otherwise use CPU.
    #[default]
    Auto,
    /// Run reranker inference on CPU, even in a CUDA-capable binary.
    Cpu,
    /// Require a CUDA-backed session; never fall back to a CPU session.
    Cuda,
}

/// Numeric precision of the reranker model artifact.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RerankerPrecision {
    /// Fused FP32 model. Portable across CPU and CUDA execution providers.
    #[default]
    Fp32,
    /// Fused FP16 model. Supported only with an explicitly required CUDA provider.
    Fp16,
}

impl fmt::Display for RerankerPrecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fp32 => f.write_str("fp32"),
            Self::Fp16 => f.write_str("fp16"),
        }
    }
}

impl FromStr for RerankerPrecision {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fp32" => Ok(Self::Fp32),
            "fp16" => Ok(Self::Fp16),
            other => Err(ParseEnumError(format!("unknown reranker precision {other:?}, expected \"fp32\" or \"fp16\""))),
        }
    }
}

impl fmt::Display for RerankerExecutionProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Cpu => f.write_str("cpu"),
            Self::Cuda => f.write_str("cuda"),
        }
    }
}

impl FromStr for RerankerExecutionProvider {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "cuda" => Ok(Self::Cuda),
            other => Err(ParseEnumError(format!(
                "unknown reranker execution provider {other:?}, expected \"auto\", \"cpu\", or \"cuda\""
            ))),
        }
    }
}

/// Database backend for memory persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DatabaseBackend {
    /// Local SQLite database with sqlite-vec.
    Sqlite,
    /// `PostgreSQL` database with `pgvector`.
    Postgres,
}

impl fmt::Display for DatabaseBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite => f.write_str("sqlite"),
            Self::Postgres => f.write_str("postgres"),
        }
    }
}

impl FromStr for DatabaseBackend {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            other => Err(ParseEnumError(format!("unknown database backend {other:?}, expected \"sqlite\" or \"postgres\""))),
        }
    }
}

/// Transport protocol for the MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Transport {
    /// MCP over stdin/stdout.
    Stdio,
    /// Streamable HTTP with SSE.
    Http,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => f.write_str("stdio"),
            Self::Http => f.write_str("http"),
        }
    }
}

impl FromStr for Transport {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stdio" => Ok(Self::Stdio),
            "http" => Ok(Self::Http),
            other => Err(ParseEnumError(format!("unknown transport {other:?}, expected \"stdio\" or \"http\""))),
        }
    }
}

/// Source of the authenticated principal for HTTP requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HttpPrincipalMode {
    /// Every valid endpoint bearer token receives one configured identity.
    #[default]
    Fixed,
    /// A separately authenticated reverse proxy asserts identity in a header.
    TrustedProxy,
}

impl fmt::Display for HttpPrincipalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed => f.write_str("fixed"),
            Self::TrustedProxy => f.write_str("trusted_proxy"),
        }
    }
}

impl FromStr for HttpPrincipalMode {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fixed" => Ok(Self::Fixed),
            "trusted_proxy" => Ok(Self::TrustedProxy),
            other => Err(ParseEnumError(format!("unknown HTTP principal mode {other:?}, expected \"fixed\" or \"trusted_proxy\""))),
        }
    }
}

/// Authorization behavior when no trusted principal is configured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnonymousPolicy {
    /// Anonymous callers may read public memories but may not write.
    #[default]
    PublicReadOnly,
    /// Anonymous callers may not read or write through the agent API.
    DenyAll,
    /// Anonymous callers may read public memories and create public memories
    /// under the fixed anonymous principal.
    PublicReadWrite,
}

impl fmt::Display for AnonymousPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PublicReadOnly => f.write_str("public_read_only"),
            Self::DenyAll => f.write_str("deny_all"),
            Self::PublicReadWrite => f.write_str("public_read_write"),
        }
    }
}

impl FromStr for AnonymousPolicy {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "public_read_only" => Ok(Self::PublicReadOnly),
            "deny_all" => Ok(Self::DenyAll),
            "public_read_write" => Ok(Self::PublicReadWrite),
            other => Err(ParseEnumError(format!(
                "unknown anonymous policy {other:?}, expected \"public_read_only\", \"deny_all\", or \"public_read_write\""
            ))),
        }
    }
}

/// Top-level configuration for the `LocalHold` server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Config {
    /// Database settings.
    pub database: DatabaseConfig,
    /// Embedding provider settings.
    pub embedding: EmbeddingConfig,
    /// MCP server transport settings.
    pub server: ServerConfig,
    /// Operational limits and validation caps.
    pub limits: LimitsConfig,
    /// Search behavior (hybrid search, RRF fusion).
    pub search: SearchConfig,
}

/// A validated configuration and the optional file that supplied it.
pub type ConfigWithSource = (Config, Option<PathBuf>);

/// Configuration for hybrid search behavior and RRF fusion parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SearchConfig {
    /// RRF constant `k`. Higher values reduce the influence of high-ranking outliers.
    /// Standard value from the original RRF paper is 60.
    pub rrf_k: u32,
    /// Weight for semantic (embedding) search results in RRF fusion.
    /// Set to 0.0 to effectively disable the semantic path in hybrid mode.
    pub rrf_semantic_weight: f64,
    /// Weight for FTS5 keyword search results in RRF fusion.
    /// Set to 0.0 to effectively disable the keyword path in hybrid mode.
    pub rrf_keyword_weight: f64,
    /// Default search mode when the caller does not specify one.
    pub default_mode: SearchMode,
    /// Candidate depth for semantic retrieval before fusion.
    pub semantic_candidate_k: usize,
    /// Candidate depth for keyword retrieval before fusion.
    pub keyword_candidate_k: usize,
    /// Number of fused candidates to pass through second-stage reranking.
    pub rerank_top_m: usize,

    // -- Composite scoring weights --
    /// Weight for the relevance component in composite scoring.
    pub relevance_weight: f64,
    /// Weight for the activity component in composite scoring.
    /// Activity reflects real use (reads, citations, confirmations), not search impressions.
    #[serde(alias = "recency_weight")]
    pub activity_weight: f64,
    /// Weight for the importance component in composite scoring.
    pub importance_weight: f64,
    /// Half-life in hours for activity decay. After this many hours since the
    /// last real use event, the stored activity mass halves.
    #[serde(alias = "recency_half_life_hours")]
    pub activity_half_life_hours: f64,
    /// Saturation threshold for activity scoring. Roughly this many effective
    /// recent uses saturate the activity boost to 1.0.
    pub activity_saturation: f64,
    /// Weight for the freshness component in composite scoring.
    /// Freshness reflects how recently the memory's content was modified.
    pub freshness_weight: f64,
    /// Freshness half-life in days for `semantic` memories.
    pub freshness_half_life_semantic_days: f64,
    /// Freshness half-life in days for `episodic` memories.
    pub freshness_half_life_episodic_days: f64,
    /// Freshness half-life in days for `procedural` memories.
    pub freshness_half_life_procedural_days: f64,
    /// Weight for the confidence component in composite scoring.
    pub confidence_weight: f64,
    /// Query relevance threshold below which results are penalized.
    /// Results with `Q(d) < relevance_floor` have their composite score
    /// multiplied by `relevance_floor_penalty`.
    pub relevance_floor: f64,
    /// Multiplicative penalty applied when `Q(d) < relevance_floor`.
    /// Default 0.25 means low-relevance results score at 25% of their
    /// normal composite, pushing them to the bottom without excluding them.
    pub relevance_floor_penalty: f64,
    /// Multiplicative penalty applied to superseded memories' composite score.
    pub superseded_penalty: f64,
    /// Cross-encoder reranker settings (nested `[search.reranker]` section).
    pub reranker: RerankerConfig,
    /// Duplicate suppression settings (nested `[search.duplicate_suppression]` section).
    pub duplicate_suppression: DuplicateSuppressionConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60,
            rrf_semantic_weight: 1.0,
            rrf_keyword_weight: 1.0,
            default_mode: SearchMode::Auto,
            semantic_candidate_k: 100,
            keyword_candidate_k: 100,
            rerank_top_m: 50,
            // Weights sum to 100 — score is on a 0-100 scale.
            relevance_weight: 60.0,
            importance_weight: 15.0,
            freshness_weight: 10.0,
            activity_weight: 10.0,
            confidence_weight: 5.0,
            activity_half_life_hours: 720.0,
            activity_saturation: 8.0,
            freshness_half_life_semantic_days: 180.0,
            freshness_half_life_episodic_days: 14.0,
            freshness_half_life_procedural_days: 365.0,
            relevance_floor: 0.15,
            relevance_floor_penalty: 0.25,
            superseded_penalty: 0.05,
            reranker: RerankerConfig::default(),
            duplicate_suppression: DuplicateSuppressionConfig::default(),
        }
    }
}

/// Duplicate suppression configuration.
///
/// When enabled, a post-ranking diversity pass penalizes results whose
/// embeddings are too similar to already-selected results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct DuplicateSuppressionConfig {
    /// Enable duplicate suppression. Default `false` (opt-in).
    pub enabled: bool,
    /// Penalty coefficient. Higher values penalize near-duplicates more aggressively.
    pub lambda: f64,
}

impl Default for DuplicateSuppressionConfig {
    fn default() -> Self {
        Self { enabled: false, lambda: 0.10_f64 }
    }
}

/// Cross-encoder reranker configuration.
///
/// Controls whether a cross-encoder model is loaded for precision reranking
/// of search results. Disabled by default — opt-in until ONNX runtime
/// integration is verified on all target platforms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RerankerConfig {
    /// Enable cross-encoder reranking. Default `false` (opt-in).
    pub enabled: bool,
    /// Execution-provider selection policy. Default [`RerankerExecutionProvider::Auto`].
    pub execution_provider: RerankerExecutionProvider,
    /// Model artifact precision. FP32 is the portable default; FP16 requires explicit CUDA.
    pub precision: RerankerPrecision,
    /// Fail startup unless the reranker initializes and passes health inference.
    pub required: bool,
    /// Model identifier for the cross-encoder and for custom `HuggingFace` downloads.
    pub model: String,
    /// Immutable source revision/commit. Required for custom auto-downloaded models.
    pub revision: String,
    /// Override: local filesystem path to a pre-exported ONNX model file.
    /// When non-empty, takes precedence over downloading from `model`.
    pub model_path: String,
    /// Expected SHA-256 for `model.onnx` when auto-downloading.
    /// Required for custom auto-downloaded models.
    pub model_sha256: String,
    /// Expected SHA-256 for `tokenizer.json` when auto-downloading.
    /// Required for custom auto-downloaded models.
    pub tokenizer_sha256: String,
    /// **Deprecated**: use `search.rerank_top_m` instead.
    ///
    /// Backward-compatibility shim for old configs. When `search.rerank_top_m`
    /// is left at its default and this field is set to a non-default value,
    /// the value is copied into `rerank_top_m` during config validation.
    /// This field has no runtime effect on the reranker itself.
    pub pool_size: usize,
    /// Blend weight for the reranker score in the final composite.
    /// `final_Q = blend_weight * R(d) + (1 - blend_weight) * H(d)`
    pub blend_weight: f64,
    /// Directory for caching downloaded ONNX model files.
    pub cache_dir: String,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            execution_provider: RerankerExecutionProvider::Auto,
            precision: RerankerPrecision::Fp32,
            required: false,
            model: DEFAULT_RERANKER_MODEL.into(),
            revision: String::new(),
            model_path: String::new(),
            model_sha256: String::new(),
            tokenizer_sha256: String::new(),
            pool_size: 50,
            blend_weight: 0.7_f64,
            cache_dir: "~/.cache/localhold/models".into(),
        }
    }
}

/// Database configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct DatabaseConfig {
    /// Active database backend.
    pub backend: DatabaseBackend,
    /// SQLite database settings.
    pub sqlite: SqliteDatabaseConfig,
    /// `PostgreSQL` database settings.
    pub postgres: PostgresDatabaseConfig,
    /// Backward-compatible alias for `database.sqlite.path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// SQLite database configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SqliteDatabaseConfig {
    /// Path to the SQLite database file.
    pub path: PathBuf,
}

/// `PostgreSQL` database configuration.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct PostgresDatabaseConfig {
    /// `PostgreSQL` connection URL.
    pub url: String,
    /// Maximum pooled connections.
    pub max_connections: u32,
    /// Whether startup should create/migrate the schema.
    pub auto_migrate: bool,
}

impl fmt::Debug for PostgresDatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresDatabaseConfig")
            .field("url", &"[REDACTED]")
            .field("max_connections", &self.max_connections)
            .field("auto_migrate", &self.auto_migrate)
            .finish()
    }
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ServerConfig {
    /// Transport protocol (`stdio` or `http`).
    pub transport: Transport,
    /// Log verbosity level (e.g. `"info"`, `"debug"`).
    pub log_level: String,
    /// Trusted launch-time principal used by non-HTTP transports.
    /// Set to an empty string or omit via env override to run anonymously.
    pub principal: Option<String>,
    /// Behavior when no trusted principal is configured.
    pub anonymous_policy: AnonymousPolicy,
    /// Bind address for HTTP transport (ignored for stdio).
    pub host: String,
    /// Bind port for HTTP transport (ignored for stdio).
    pub port: u16,
    /// Exact URL path for HTTP transport (ignored for stdio).
    pub path: String,
    /// Bearer token required for every request to the HTTP MCP endpoint.
    /// Empty or omitted leaves the endpoint unauthenticated; `anonymous_policy`
    /// then governs access because HTTP never inherits the launch principal.
    pub http_auth_token: Option<String>,
    /// How bearer-authenticated HTTP requests obtain their principal.
    pub http_principal_mode: HttpPrincipalMode,
    /// Fixed principal assigned in `fixed` mode.
    pub http_principal: String,
    /// Identity header trusted only in `trusted_proxy` mode.
    pub http_principal_header: String,
    /// Host header values accepted by the HTTP transport's DNS-rebinding guard.
    pub http_allowed_hosts: Vec<String>,
    /// Maximum request body size for HTTP transport in bytes.
    pub max_body_bytes: usize,
    /// Maximum number of active stateful HTTP MCP sessions.
    pub http_max_sessions: usize,
    /// Close stateful HTTP MCP sessions after this many idle seconds.
    pub http_session_idle_timeout_secs: u64,
    /// Expose privileged `admin_*` maintenance tools.
    pub admin_tools_enabled: bool,
}

/// Operational limits and validation caps.
///
/// These control maximum sizes, timeouts, and defaults for the MCP tool
/// handlers and embedding provider. All fields have sensible defaults
/// matching the original hard-coded constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct LimitsConfig {
    /// Maximum number of results for `recall`.
    /// Default: 200 (balances result quality with response size; higher values
    /// increase latency from distance re-ranking).
    pub max_search_limit: usize,
    /// Maximum first-stage candidates fetched before reranking and composite scoring.
    /// Default: 1,000 (matches current vector backend safety ceiling).
    pub max_candidate_pool_size: usize,
    /// Maximum number of results for `admin_list`.
    /// Default: 500 (higher than search because listing skips vector distance
    /// computation and returns lightweight metadata).
    pub max_list_limit: usize,
    /// Maximum content size in bytes.
    /// Default: 65,536 (64 `KiB`, fits within typical embedding model context
    /// windows such as nomic-embed-text's 8,192-token limit).
    pub max_content_length: usize,
    /// Maximum number of tags per memory.
    /// Default: 50 (prevents tag explosion that degrades filter performance
    /// while remaining generous for real-world use).
    pub max_tags_per_memory: usize,
    /// Maximum length of a single tag in bytes.
    /// Default: 256 (long enough for namespaced tags like
    /// `project:localhold/component:config` while preventing abuse).
    pub max_tag_length: usize,
    /// Maximum items in a single `memory_batch_store` request.
    /// Default: 100 (keeps transaction size bounded to avoid long-held write
    /// locks on the SQLite database).
    pub max_batch_size: usize,
    /// Maximum bulk re-embed limit per `memory_reembed` invocation.
    /// Default: 100 (caps the number of embedding calls per request to
    /// prevent timeouts and excessive resource consumption).
    pub max_reembed_limit: usize,
    /// Timeout in seconds for embedding HTTP requests and health checks.
    /// Default: 30 (generous enough for cold-start model loading while still
    /// surfacing genuine connectivity failures).
    pub embedding_timeout_secs: u64,
    /// Maximum embedding requests allowed to run concurrently.
    /// Default: 8 (bounds local accelerator load and hosted-provider pressure).
    pub max_concurrent_embedding_requests: usize,
    /// Maximum texts sent in one embedding provider request.
    /// Default: 32 (amortizes request overhead without oversized payloads).
    pub embedding_batch_size: usize,
    /// Number of retries after an initial retryable embedding failure.
    pub embedding_max_retries: u32,
    /// Delay before the first embedding retry, in milliseconds.
    pub embedding_retry_initial_backoff_ms: u64,
    /// Maximum client or provider-directed retry delay, in milliseconds.
    pub embedding_retry_max_backoff_ms: u64,
    /// Graceful shutdown timeout in seconds for draining background tasks.
    /// Default: 10 (long enough for in-flight embeddings to finish while
    /// preventing indefinite hangs on unresponsive providers).
    pub shutdown_timeout_secs: u64,
    /// Maximum value of `top_tags_limit` in `memory_count` requests.
    /// Default: 100 (caps the tag-breakdown query to avoid scanning the
    /// entire tag space on large databases).
    pub max_top_tags_limit: usize,
    /// Maximum number of audit-history rows returned by `memory_history`.
    /// Default: 500 (keeps large audit trails queryable while bounding
    /// single-request memory and latency spikes independently of list/search).
    pub max_history_limit: usize,
    /// Maximum ANN neighbors to check per memory during consolidation.
    /// Controls BFS frontier expansion breadth. Higher values find more
    /// transitive duplicates but increase query count per memory.
    /// Default: 20.
    pub consolidation_neighbor_limit: usize,
    /// Maximum number of entities per memory.
    /// Default: 50 (parity with `max_tags_per_memory`).
    pub max_entities_per_memory: usize,
    /// Maximum length of an entity name or type in bytes.
    /// Default: 256 (parity with `max_tag_length`).
    pub max_entity_field_length: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_search_limit: 50,
            max_candidate_pool_size: MAX_CANDIDATE_POOL_SIZE_CEILING,
            max_list_limit: 500,
            max_content_length: 65_536,
            max_tags_per_memory: 50,
            max_tag_length: 256,
            max_batch_size: 100,
            max_reembed_limit: 100,
            embedding_timeout_secs: 30,
            max_concurrent_embedding_requests: 8,
            embedding_batch_size: 32,
            embedding_max_retries: 2,
            embedding_retry_initial_backoff_ms: 500,
            embedding_retry_max_backoff_ms: 30_000,
            shutdown_timeout_secs: 10,
            max_top_tags_limit: 100,
            max_history_limit: 500,
            consolidation_neighbor_limit: 20,
            max_entities_per_memory: 50,
            max_entity_field_length: 256,
        }
    }
}

#[expect(unused_must_use, reason = "best-effort stderr warning before tracing is ready")]
fn default_sqlite_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            writeln!(std::io::stderr(), "warning: could not determine local data directory, falling back to CWD");
            PathBuf::from(".")
        })
        .join("localhold")
        .join("localhold.db")
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            backend: DatabaseBackend::Sqlite,
            sqlite: SqliteDatabaseConfig::default(),
            postgres: PostgresDatabaseConfig::default(),
            path: None,
        }
    }
}

impl Default for SqliteDatabaseConfig {
    fn default() -> Self {
        Self { path: default_sqlite_path() }
    }
}

impl Default for PostgresDatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://localhold:localhold@localhost:5432/localhold".into(),
            max_connections: 5,
            auto_migrate: true,
        }
    }
}

impl DatabaseConfig {
    /// Path to the configured SQLite database file.
    #[must_use]
    pub fn sqlite_path(&self) -> &Path {
        self.path.as_deref().unwrap_or(&self.sqlite.path)
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self::Noop {
            dimensions: DEFAULT_EMBEDDING_DIMENSIONS,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Stdio,
            log_level: "info".into(),
            principal: Some("stdio".into()),
            anonymous_policy: AnonymousPolicy::PublicReadOnly,
            host: "127.0.0.1".into(),
            port: 8080,
            path: "/mcp".into(),
            http_auth_token: None,
            http_principal_mode: HttpPrincipalMode::Fixed,
            http_principal: "http".into(),
            http_principal_header: DEFAULT_HTTP_PRINCIPAL_HEADER.into(),
            http_allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            max_body_bytes: DEFAULT_HTTP_MAX_BODY_BYTES,
            http_max_sessions: DEFAULT_HTTP_MAX_SESSIONS,
            http_session_idle_timeout_secs: DEFAULT_HTTP_SESSION_IDLE_TIMEOUT_SECS,
            admin_tools_enabled: false,
        }
    }
}

/// Look up `var` in the env map and parse it into `target`.
///
/// Errors identify the variable but never include its potentially sensitive
/// value.
fn apply_parsed_env<T: FromStr>(env: &HashMap<String, String>, var: &str, target: &mut T) -> Result<(), EngineError> {
    if let Some(v) = env.get(var) {
        *target = v.parse().map_err(|_error| EngineError::config(format!("{var} environment override is malformed")))?;
    }
    Ok(())
}

fn normalize_http_header_name(value: &str) -> Result<String, EngineError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(EngineError::config("server.http_principal_header must not be empty"));
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'))
    {
        return Err(EngineError::config(format!("server.http_principal_header is not a valid HTTP header name: {trimmed:?}")));
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn validate_http_path(path: &str) -> Result<(), EngineError> {
    if !path.starts_with('/') {
        return Err(EngineError::config("server.path must start with '/'"));
    }
    if !path.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(EngineError::config("server.path must contain only visible ASCII characters"));
    }
    if path.contains(['?', '#']) {
        return Err(EngineError::config("server.path must not contain a query string or fragment"));
    }
    if path != "/" {
        for segment in path.split('/').skip(1) {
            if segment.is_empty() {
                return Err(EngineError::config("server.path must not contain empty segments or end with '/'"));
            }
            if segment.starts_with([':', '*']) || segment.contains(['{', '}']) {
                return Err(EngineError::config("server.path must be a static path without parameters or wildcards"));
            }
        }
    }
    Ok(())
}

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// Returns `Err` when the path starts with `~` but the home directory
/// cannot be determined — failing fast prevents silently creating a
/// literal `~` directory under the working directory.
///
/// Paths that do not start with `~` are returned as-is.
pub(crate) fn expand_tilde(path: &str) -> Result<PathBuf, EngineError> {
    if path == "~" {
        return dirs::home_dir().ok_or_else(|| EngineError::config("path starts with ~ but home directory cannot be determined"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or_else(|| EngineError::config("path starts with ~/ but home directory cannot be determined"))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(path))
}

impl Config {
    /// Load config from the platform user config directory + env overrides.
    ///
    /// Reads `localhold/localhold.toml` within the platform config directory.
    /// Files in the current working directory are never loaded implicitly.
    /// A missing file is not an error; defaults apply.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Config` if a config file exists but cannot be read or parsed.
    pub fn load() -> Result<Self, EngineError> {
        Self::load_with_source().map(|(config, _source)| config)
    }

    /// Load config and report the user config file that supplied it, if any.
    ///
    /// Environment overrides are applied after the file is loaded. The source
    /// path is safe to report, but callers must not serialize the config itself
    /// because it can contain credentials.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Config` under the same conditions as [`Self::load`].
    pub fn load_with_source() -> Result<ConfigWithSource, EngineError> {
        let config_dir = user_config_dir();
        let candidates = user_config_candidates(config_dir.as_deref());
        let env_map = collect_localhold_env_vars();
        Self::load_from_sources_with_source(&candidates, &env_map)
    }

    /// Load config from explicitly provided file search paths and environment map.
    ///
    /// This is the pure-function core of config loading: it does not touch the
    /// process CWD, real environment variables, or platform config directories.
    /// [`Config::load()`] wraps this with real sources; tests can call it
    /// directly with synthetic inputs.
    ///
    /// The first existing path in `paths` wins. Entries that do not exist on
    /// disk are silently skipped.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Config` if a config file exists but cannot be read or parsed,
    /// or if validation fails.
    pub fn load_from_sources(paths: &[PathBuf], env_map: &HashMap<String, String>) -> Result<Self, EngineError> {
        Self::load_from_sources_with_source(paths, env_map).map(|(config, _source)| config)
    }

    fn load_from_sources_with_source(paths: &[PathBuf], env_map: &HashMap<String, String>) -> Result<ConfigWithSource, EngineError> {
        let mut config = None;
        let mut source = None;
        for candidate in paths {
            if candidate.exists() {
                config = Some(Self::load_from_file(candidate)?);
                source = Some(candidate.clone());
                break;
            }
        }
        let mut config = config.unwrap_or_default();
        config.apply_env_from_map(env_map)?;
        config.resolve_paths()?;
        config.validate(env_map)?;
        Ok((config, source))
    }

    fn load_from_file(path: &Path) -> Result<Self, EngineError> {
        let contents = std::fs::read_to_string(path).map_err(|e| EngineError::config(format!("reading {}: {e}", path.display())))?;
        toml::from_str(&contents).map_err(|e| EngineError::config(format!("parsing {}: {e}", path.display())))
    }

    #[expect(clippy::too_many_lines, reason = "centralized config env override table is intentionally linear")]
    fn apply_env_from_map(&mut self, env: &HashMap<String, String>) -> Result<(), EngineError> {
        apply_parsed_env(env, "LOCALHOLD_DB_BACKEND", &mut self.database.backend)?;
        if let Some(v) = env.get("LOCALHOLD_DB_PATH") {
            self.database.path = Some(PathBuf::from(v));
        }
        if let Some(v) = env.get("LOCALHOLD_POSTGRES_URL") {
            self.database.postgres.url.clone_from(v);
        }
        apply_parsed_env(env, "LOCALHOLD_POSTGRES_MAX_CONNECTIONS", &mut self.database.postgres.max_connections)?;
        apply_parsed_env(env, "LOCALHOLD_POSTGRES_AUTO_MIGRATE", &mut self.database.postgres.auto_migrate)?;
        apply_embedding_env(&mut self.embedding, env)?;
        if let Some(v) = env.get("LOCALHOLD_LOG_LEVEL") {
            self.server.log_level.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_PRINCIPAL") {
            self.server.principal = Some(v.clone());
        }
        apply_parsed_env(env, "LOCALHOLD_ANONYMOUS_POLICY", &mut self.server.anonymous_policy)?;
        apply_parsed_env(env, "LOCALHOLD_TRANSPORT", &mut self.server.transport)?;
        if let Some(v) = env.get("LOCALHOLD_HTTP_HOST") {
            self.server.host.clone_from(v);
        }
        apply_parsed_env(env, "LOCALHOLD_HTTP_PORT", &mut self.server.port)?;
        if let Some(v) = env.get("LOCALHOLD_HTTP_PATH") {
            self.server.path.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_HTTP_AUTH_TOKEN") {
            self.server.http_auth_token = Some(v.clone());
        }
        apply_parsed_env(env, "LOCALHOLD_HTTP_PRINCIPAL_MODE", &mut self.server.http_principal_mode)?;
        if let Some(v) = env.get("LOCALHOLD_HTTP_PRINCIPAL") {
            self.server.http_principal.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_HTTP_PRINCIPAL_HEADER") {
            self.server.http_principal_header.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_HTTP_ALLOWED_HOSTS") {
            self.server.http_allowed_hosts = v.split(',').map(str::trim).filter(|host| !host.is_empty()).map(ToOwned::to_owned).collect();
        }
        apply_parsed_env(env, "LOCALHOLD_HTTP_MAX_BODY_BYTES", &mut self.server.max_body_bytes)?;
        apply_parsed_env(env, "LOCALHOLD_HTTP_MAX_SESSIONS", &mut self.server.http_max_sessions)?;
        apply_parsed_env(env, "LOCALHOLD_HTTP_SESSION_IDLE_TIMEOUT_SECS", &mut self.server.http_session_idle_timeout_secs)?;
        apply_parsed_env(env, "LOCALHOLD_ADMIN_TOOLS_ENABLED", &mut self.server.admin_tools_enabled)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_SEARCH_LIMIT", &mut self.limits.max_search_limit)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_CANDIDATE_POOL_SIZE", &mut self.limits.max_candidate_pool_size)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_LIST_LIMIT", &mut self.limits.max_list_limit)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_CONTENT_LENGTH", &mut self.limits.max_content_length)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_TAGS_PER_MEMORY", &mut self.limits.max_tags_per_memory)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_TAG_LENGTH", &mut self.limits.max_tag_length)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_BATCH_SIZE", &mut self.limits.max_batch_size)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_REEMBED_LIMIT", &mut self.limits.max_reembed_limit)?;
        apply_parsed_env(env, "LOCALHOLD_EMBEDDING_TIMEOUT", &mut self.limits.embedding_timeout_secs)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_CONCURRENT_EMBEDDING_REQUESTS", &mut self.limits.max_concurrent_embedding_requests)?;
        apply_parsed_env(env, "LOCALHOLD_EMBEDDING_BATCH_SIZE", &mut self.limits.embedding_batch_size)?;
        apply_parsed_env(env, "LOCALHOLD_EMBEDDING_MAX_RETRIES", &mut self.limits.embedding_max_retries)?;
        apply_parsed_env(env, "LOCALHOLD_EMBEDDING_RETRY_INITIAL_BACKOFF_MS", &mut self.limits.embedding_retry_initial_backoff_ms)?;
        apply_parsed_env(env, "LOCALHOLD_EMBEDDING_RETRY_MAX_BACKOFF_MS", &mut self.limits.embedding_retry_max_backoff_ms)?;
        apply_parsed_env(env, "LOCALHOLD_SHUTDOWN_TIMEOUT", &mut self.limits.shutdown_timeout_secs)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_TOP_TAGS_LIMIT", &mut self.limits.max_top_tags_limit)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_HISTORY_LIMIT", &mut self.limits.max_history_limit)?;
        apply_parsed_env(env, "LOCALHOLD_CONSOLIDATION_NEIGHBOR_LIMIT", &mut self.limits.consolidation_neighbor_limit)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_ENTITIES_PER_MEMORY", &mut self.limits.max_entities_per_memory)?;
        apply_parsed_env(env, "LOCALHOLD_MAX_ENTITY_FIELD_LENGTH", &mut self.limits.max_entity_field_length)?;
        apply_parsed_env(env, "LOCALHOLD_RRF_K", &mut self.search.rrf_k)?;
        apply_parsed_env(env, "LOCALHOLD_RRF_SEMANTIC_WEIGHT", &mut self.search.rrf_semantic_weight)?;
        apply_parsed_env(env, "LOCALHOLD_RRF_KEYWORD_WEIGHT", &mut self.search.rrf_keyword_weight)?;
        apply_parsed_env(env, "LOCALHOLD_DEFAULT_SEARCH_MODE", &mut self.search.default_mode)?;
        apply_parsed_env(env, "LOCALHOLD_SEMANTIC_CANDIDATE_K", &mut self.search.semantic_candidate_k)?;
        apply_parsed_env(env, "LOCALHOLD_KEYWORD_CANDIDATE_K", &mut self.search.keyword_candidate_k)?;
        apply_parsed_env(env, "LOCALHOLD_RERANK_TOP_M", &mut self.search.rerank_top_m)?;
        apply_parsed_env(env, "LOCALHOLD_RERANKER_POOL_SIZE", &mut self.search.reranker.pool_size)?;
        apply_parsed_env(env, "LOCALHOLD_RELEVANCE_WEIGHT", &mut self.search.relevance_weight)?;
        // Backward compat: apply deprecated aliases first so the new name
        // takes precedence when both are set during migration.
        apply_parsed_env(env, "LOCALHOLD_RECENCY_WEIGHT", &mut self.search.activity_weight)?;
        apply_parsed_env(env, "LOCALHOLD_ACTIVITY_WEIGHT", &mut self.search.activity_weight)?;
        apply_parsed_env(env, "LOCALHOLD_IMPORTANCE_WEIGHT", &mut self.search.importance_weight)?;
        apply_parsed_env(env, "LOCALHOLD_RECENCY_HALF_LIFE_HOURS", &mut self.search.activity_half_life_hours)?;
        apply_parsed_env(env, "LOCALHOLD_ACTIVITY_HALF_LIFE_HOURS", &mut self.search.activity_half_life_hours)?;
        apply_parsed_env(env, "LOCALHOLD_ACTIVITY_SATURATION", &mut self.search.activity_saturation)?;
        apply_parsed_env(env, "LOCALHOLD_FRESHNESS_WEIGHT", &mut self.search.freshness_weight)?;
        apply_parsed_env(env, "LOCALHOLD_FRESHNESS_HALF_LIFE_SEMANTIC_DAYS", &mut self.search.freshness_half_life_semantic_days)?;
        apply_parsed_env(env, "LOCALHOLD_FRESHNESS_HALF_LIFE_EPISODIC_DAYS", &mut self.search.freshness_half_life_episodic_days)?;
        apply_parsed_env(env, "LOCALHOLD_FRESHNESS_HALF_LIFE_PROCEDURAL_DAYS", &mut self.search.freshness_half_life_procedural_days)?;
        apply_parsed_env(env, "LOCALHOLD_CONFIDENCE_WEIGHT", &mut self.search.confidence_weight)?;
        apply_parsed_env(env, "LOCALHOLD_RELEVANCE_FLOOR", &mut self.search.relevance_floor)?;
        apply_parsed_env(env, "LOCALHOLD_RELEVANCE_FLOOR_PENALTY", &mut self.search.relevance_floor_penalty)?;
        apply_parsed_env(env, "LOCALHOLD_SUPERSEDED_PENALTY", &mut self.search.superseded_penalty)?;
        apply_parsed_env(env, "LOCALHOLD_RERANKER_ENABLED", &mut self.search.reranker.enabled)?;
        apply_parsed_env(env, "LOCALHOLD_RERANKER_EXECUTION_PROVIDER", &mut self.search.reranker.execution_provider)?;
        apply_parsed_env(env, "LOCALHOLD_RERANKER_PRECISION", &mut self.search.reranker.precision)?;
        apply_parsed_env(env, "LOCALHOLD_RERANKER_REQUIRED", &mut self.search.reranker.required)?;
        if let Some(v) = env.get("LOCALHOLD_RERANKER_MODEL") {
            self.search.reranker.model.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_RERANKER_REVISION") {
            self.search.reranker.revision.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_RERANKER_MODEL_PATH") {
            self.search.reranker.model_path.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_RERANKER_MODEL_SHA256") {
            self.search.reranker.model_sha256.clone_from(v);
        }
        if let Some(v) = env.get("LOCALHOLD_RERANKER_TOKENIZER_SHA256") {
            self.search.reranker.tokenizer_sha256.clone_from(v);
        }
        apply_parsed_env(env, "LOCALHOLD_RERANKER_BLEND_WEIGHT", &mut self.search.reranker.blend_weight)?;
        if let Some(v) = env.get("LOCALHOLD_RERANKER_CACHE_DIR") {
            self.search.reranker.cache_dir.clone_from(v);
        }
        apply_parsed_env(env, "LOCALHOLD_DUPLICATE_SUPPRESSION_ENABLED", &mut self.search.duplicate_suppression.enabled)?;
        apply_parsed_env(env, "LOCALHOLD_DUPLICATE_SUPPRESSION_LAMBDA", &mut self.search.duplicate_suppression.lambda)?;
        Ok(())
    }

    fn resolve_paths(&mut self) -> Result<(), EngineError> {
        if let Some(path) = &mut self.database.path {
            expand_tilde_path(path)?;
        }
        expand_tilde_path(&mut self.database.sqlite.path)?;
        Ok(())
    }

    fn validate(&mut self, env: &HashMap<String, String>) -> Result<(), EngineError> {
        validate_database_config(&mut self.database)?;
        normalize_embedding_config(&mut self.embedding);
        validate_embedding_config(&self.embedding)?;
        self.server.principal = self
            .server
            .principal
            .as_deref()
            .map(str::trim)
            .filter(|principal| !principal.is_empty())
            .map(ToOwned::to_owned);
        self.server.http_auth_token = self
            .server
            .http_auth_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToOwned::to_owned);
        self.server.http_principal = self.server.http_principal.trim().to_owned();
        self.server.http_principal_header = normalize_http_header_name(&self.server.http_principal_header)?;
        self.server.http_allowed_hosts = self
            .server
            .http_allowed_hosts
            .iter()
            .map(|host| host.trim())
            .filter(|host| !host.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        validate_server_config(&self.server)?;
        self.search.reranker.model = self.search.reranker.model.trim().to_owned();
        self.search.reranker.revision = self.search.reranker.revision.trim().to_owned();
        self.search.reranker.model_path = self.search.reranker.model_path.trim().to_owned();
        self.search.reranker.model_sha256 = self.search.reranker.model_sha256.trim().to_owned();
        self.search.reranker.tokenizer_sha256 = self.search.reranker.tokenizer_sha256.trim().to_owned();
        self.search.reranker.cache_dir = self.search.reranker.cache_dir.trim().to_owned();
        // Backward compat: honor deprecated pool_size when rerank_top_m
        // was not explicitly set via TOML or env var.
        let rerank_top_m_explicit = self.search.rerank_top_m != SearchConfig::default().rerank_top_m || env.contains_key("LOCALHOLD_RERANK_TOP_M");
        if !rerank_top_m_explicit && self.search.reranker.pool_size != RerankerConfig::default().pool_size {
            self.search.rerank_top_m = self.search.reranker.pool_size;
        }
        validate_limits_config(&self.limits)?;
        validate_search_config(&self.search)?;
        Ok(())
    }
}

pub(super) fn user_config_candidates(config_dir: Option<&Path>) -> Vec<PathBuf> {
    config_dir.map_or_else(Vec::new, |dir| vec![dir.join("localhold/localhold.toml")])
}

pub(super) fn user_config_dir() -> Option<PathBuf> {
    #[cfg(any(test, feature = "testing"))]
    if let Some(path) = std::env::var_os("LOCALHOLD_TEST_CONFIG_DIR") {
        return Some(path.into());
    }
    dirs::config_dir()
}

pub(crate) fn validate_server_config(config: &ServerConfig) -> Result<(), EngineError> {
    if config.max_body_bytes == 0 {
        return Err(EngineError::config("server.max_body_bytes must be greater than zero"));
    }
    if config.http_max_sessions == 0 {
        return Err(EngineError::config("server.http_max_sessions must be greater than zero"));
    }
    if config.http_session_idle_timeout_secs == 0 {
        return Err(EngineError::config("server.http_session_idle_timeout_secs must be greater than zero"));
    }
    if config
        .http_auth_token
        .as_deref()
        .is_some_and(|token| token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        return Err(EngineError::config("server.http_auth_token must contain only visible ASCII characters"));
    }
    if config.http_principal_mode == HttpPrincipalMode::Fixed && config.http_principal.is_empty() {
        return Err(EngineError::config("server.http_principal must not be empty in fixed mode"));
    }
    if config.transport == Transport::Http && config.http_principal_mode == HttpPrincipalMode::TrustedProxy && config.http_auth_token.is_none() {
        return Err(EngineError::config(
            "server.http_auth_token is required in trusted_proxy principal mode to prevent direct anonymous access",
        ));
    }
    validate_http_path(&config.path)?;
    if config.http_allowed_hosts.is_empty() {
        return Err(EngineError::config("server.http_allowed_hosts must contain at least one host"));
    }
    for host in &config.http_allowed_hosts {
        if host == "*" || host.bytes().any(|byte| byte.is_ascii_whitespace()) || host.contains('/') || host.contains('?') || host.contains('#') {
            return Err(EngineError::config(format!("server.http_allowed_hosts contains an invalid host: {host:?}")));
        }
        if axum::http::uri::Authority::try_from(host.as_str()).is_err() && host.parse::<std::net::IpAddr>().is_err() {
            return Err(EngineError::config(format!("server.http_allowed_hosts contains an invalid host: {host:?}")));
        }
    }
    Ok(())
}

fn validate_limits_config(config: &LimitsConfig) -> Result<(), EngineError> {
    for (field, value) in [
        ("limits.max_search_limit", config.max_search_limit),
        ("limits.max_candidate_pool_size", config.max_candidate_pool_size),
        ("limits.max_list_limit", config.max_list_limit),
        ("limits.max_content_length", config.max_content_length),
        ("limits.max_tags_per_memory", config.max_tags_per_memory),
        ("limits.max_tag_length", config.max_tag_length),
        ("limits.max_batch_size", config.max_batch_size),
        ("limits.max_reembed_limit", config.max_reembed_limit),
        ("limits.embedding_timeout_secs", usize::try_from(config.embedding_timeout_secs).unwrap_or(usize::MAX)),
        ("limits.max_concurrent_embedding_requests", config.max_concurrent_embedding_requests),
        ("limits.embedding_batch_size", config.embedding_batch_size),
        (
            "limits.embedding_retry_initial_backoff_ms",
            usize::try_from(config.embedding_retry_initial_backoff_ms).unwrap_or(usize::MAX),
        ),
        (
            "limits.embedding_retry_max_backoff_ms",
            usize::try_from(config.embedding_retry_max_backoff_ms).unwrap_or(usize::MAX),
        ),
        ("limits.shutdown_timeout_secs", usize::try_from(config.shutdown_timeout_secs).unwrap_or(usize::MAX)),
        ("limits.max_top_tags_limit", config.max_top_tags_limit),
        ("limits.max_history_limit", config.max_history_limit),
        ("limits.max_entities_per_memory", config.max_entities_per_memory),
        ("limits.max_entity_field_length", config.max_entity_field_length),
    ] {
        if value == 0 {
            return Err(EngineError::config(format!("{field} must be greater than zero")));
        }
    }
    if config.max_candidate_pool_size > MAX_CANDIDATE_POOL_SIZE_CEILING {
        return Err(EngineError::config(format!("limits.max_candidate_pool_size must be <= {MAX_CANDIDATE_POOL_SIZE_CEILING}")));
    }
    if config.max_candidate_pool_size < config.max_search_limit {
        return Err(EngineError::config("limits.max_candidate_pool_size must be >= limits.max_search_limit"));
    }
    if config.embedding_max_retries > 10 {
        return Err(EngineError::config("limits.embedding_max_retries must be <= 10"));
    }
    if config.embedding_retry_max_backoff_ms < config.embedding_retry_initial_backoff_ms {
        return Err(EngineError::config(
            "limits.embedding_retry_max_backoff_ms must be >= limits.embedding_retry_initial_backoff_ms",
        ));
    }
    Ok(())
}

fn expand_tilde_path(path: &mut PathBuf) -> Result<(), EngineError> {
    if let Some(s) = path.to_str()
        && (s.starts_with("~/") || s == "~")
    {
        *path = expand_tilde(s)?;
    }
    Ok(())
}

/// Validate database configuration for the active backend.
fn validate_database_config(config: &mut DatabaseConfig) -> Result<(), EngineError> {
    match config.backend {
        DatabaseBackend::Sqlite => {
            if config.sqlite_path().as_os_str().is_empty() {
                return Err(EngineError::config("database.sqlite.path must not be empty"));
            }
        }
        DatabaseBackend::Postgres => {
            config.postgres.url = config.postgres.url.trim().to_owned();
            if config.postgres.url.is_empty() {
                return Err(EngineError::config("database.postgres.url must not be empty"));
            }
            if !config.postgres.url.starts_with("postgres://") && !config.postgres.url.starts_with("postgresql://") {
                return Err(EngineError::config("database.postgres.url must start with postgres:// or postgresql://"));
            }
            if config.postgres.max_connections == 0 {
                return Err(EngineError::config("database.postgres.max_connections must be greater than zero"));
            }
        }
    }
    Ok(())
}

/// Validate that a floating-point value is finite and non-negative.
fn validate_non_negative_finite(value: f64, name: &str) -> Result<(), EngineError> {
    if !value.is_finite() || value < 0.0_f64 {
        return Err(EngineError::config(format!("{name} must be a finite non-negative number")));
    }
    Ok(())
}

/// Validate that a floating-point value is finite and in `[0.0, 1.0]`.
fn validate_unit_interval(value: f64, name: &str) -> Result<(), EngineError> {
    if !value.is_finite() || !(0.0_f64..=1.0_f64).contains(&value) {
        return Err(EngineError::config(format!("{name} must be a finite number in [0, 1]")));
    }
    Ok(())
}

/// Validate that a floating-point value is finite and strictly positive.
fn validate_positive_finite(value: f64, name: &str) -> Result<(), EngineError> {
    if !value.is_finite() || value <= 0.0_f64 {
        return Err(EngineError::config(format!("{name} must be a finite positive number")));
    }
    Ok(())
}

/// Validate search configuration fields.
fn validate_search_config(config: &SearchConfig) -> Result<(), EngineError> {
    if config.rrf_k == 0 {
        return Err(EngineError::config("rrf_k must be greater than zero"));
    }
    if config.semantic_candidate_k == 0 {
        return Err(EngineError::config("semantic_candidate_k must be greater than zero"));
    }
    if config.keyword_candidate_k == 0 {
        return Err(EngineError::config("keyword_candidate_k must be greater than zero"));
    }
    if config.rerank_top_m == 0 {
        return Err(EngineError::config("rerank_top_m must be greater than zero"));
    }
    validate_non_negative_finite(config.rrf_semantic_weight, "rrf_semantic_weight")?;
    validate_non_negative_finite(config.rrf_keyword_weight, "rrf_keyword_weight")?;
    validate_non_negative_finite(config.relevance_weight, "relevance_weight")?;
    validate_non_negative_finite(config.activity_weight, "activity_weight")?;
    validate_non_negative_finite(config.importance_weight, "importance_weight")?;
    validate_non_negative_finite(config.freshness_weight, "freshness_weight")?;
    validate_non_negative_finite(config.confidence_weight, "confidence_weight")?;
    validate_unit_interval(config.relevance_floor, "relevance_floor")?;
    validate_unit_interval(config.relevance_floor_penalty, "relevance_floor_penalty")?;
    validate_unit_interval(config.superseded_penalty, "superseded_penalty")?;
    validate_non_negative_finite(config.duplicate_suppression.lambda, "duplicate_suppression.lambda")?;
    validate_positive_finite(config.activity_half_life_hours, "activity_half_life_hours")?;
    validate_positive_finite(config.activity_saturation, "activity_saturation")?;
    validate_positive_finite(config.freshness_half_life_semantic_days, "freshness_half_life_semantic_days")?;
    validate_positive_finite(config.freshness_half_life_episodic_days, "freshness_half_life_episodic_days")?;
    validate_positive_finite(config.freshness_half_life_procedural_days, "freshness_half_life_procedural_days")?;
    validate_reranker_config(&config.reranker)?;
    // Prevent subnormal values that would make the decay lambda infinite,
    // producing NaN in exp calculations when hours == 0.
    if config.activity_half_life_hours < 0.001_f64 {
        return Err(EngineError::config("activity_half_life_hours must be >= 0.001 (3.6 seconds minimum half-life)"));
    }
    // Composite scoring weights must sum to 100.0 (within floating-point tolerance).
    // Score is on a 0-100 scale: S(d) = w_q*Q + w_i*I + w_f*F + w_a*A + w_c*C
    #[expect(clippy::float_arithmetic, reason = "weight-sum validation requires arithmetic")]
    let weight_sum = config.relevance_weight + config.importance_weight + config.freshness_weight + config.activity_weight + config.confidence_weight;
    #[expect(clippy::float_arithmetic, reason = "tolerance check requires subtraction")]
    if (weight_sum - 100.0_f64).abs() > 1.0_f64 {
        return Err(EngineError::config(format!(
            "composite scoring weights (relevance + importance + freshness + activity + confidence) must sum to 100.0, got {weight_sum:.1}"
        )));
    }
    Ok(())
}

fn validate_reranker_config(config: &RerankerConfig) -> Result<(), EngineError> {
    if config.pool_size == 0 {
        return Err(EngineError::config("reranker.pool_size must be greater than zero"));
    }
    if !config.blend_weight.is_finite() || !(0.0_f64..=1.0_f64).contains(&config.blend_weight) {
        return Err(EngineError::config("reranker.blend_weight must be a finite number in [0, 1]"));
    }
    if config.required && !config.enabled {
        return Err(EngineError::config("reranker.required = true requires reranker.enabled = true"));
    }
    if config.precision == RerankerPrecision::Fp16 && config.execution_provider != RerankerExecutionProvider::Cuda {
        return Err(EngineError::config(
            "reranker.precision = \"fp16\" requires reranker.execution_provider = \"cuda\"; auto fallback and CPU execution are not supported",
        ));
    }
    if config.precision == RerankerPrecision::Fp16
        && config.model_path.is_empty()
        && (!is_builtin_default_reranker_model(&config.model)
            || (!config.revision.is_empty() && config.revision != DEFAULT_RERANKER_REVISION)
            || !config.model_sha256.is_empty()
            || !config.tokenizer_sha256.is_empty())
    {
        return Err(EngineError::config(
            "the managed fp16 reranker artifact requires the builtin model and pins; use model_path for a custom FP16 artifact",
        ));
    }
    // Validate auto-download requirements eagerly regardless of feature flag
    // so misconfigurations surface at startup, not at first search.
    if config.enabled && config.model_path.is_empty() {
        if is_builtin_default_reranker_model(&config.model) {
            // Overriding the revision on the builtin model requires explicit
            // hashes — the pinned defaults only match the pinned revision.
            let custom_revision = !config.revision.is_empty() && config.revision != DEFAULT_RERANKER_REVISION;
            if custom_revision && (config.model_sha256.is_empty() || config.tokenizer_sha256.is_empty()) {
                return Err(EngineError::config(
                    "overriding reranker.revision on the builtin model requires explicit model_sha256 and tokenizer_sha256",
                ));
            }
        } else {
            if config.revision.is_empty() {
                return Err(EngineError::config("reranker.revision must be set for custom auto-downloaded models"));
            }
            if config.model_sha256.is_empty() {
                return Err(EngineError::config("reranker.model_sha256 must be set for custom auto-downloaded models"));
            }
            if config.tokenizer_sha256.is_empty() {
                return Err(EngineError::config("reranker.tokenizer_sha256 must be set for custom auto-downloaded models"));
            }
        }
    }
    Ok(())
}

/// Check if a model identifier matches the builtin default reranker model.
///
/// Accepts both the hyphenated (`L-6`) and canonical (`L6`) forms of the
/// model name to handle the `HuggingFace` model-card vs repo-name discrepancy.
pub(crate) fn is_builtin_default_reranker_model(model: &str) -> bool {
    model == DEFAULT_RERANKER_MODEL || model == DEFAULT_RERANKER_MODEL_CANONICAL
}

/// Collect all `LOCALHOLD_*` env vars into a map for [`Config::load_from_sources`].
#[expect(clippy::too_many_lines, reason = "explicit environment allowlist is intentionally centralized")]
fn collect_localhold_env_vars() -> HashMap<String, String> {
    let keys = [
        "LOCALHOLD_DB_BACKEND",
        "LOCALHOLD_DB_PATH",
        "LOCALHOLD_POSTGRES_URL",
        "LOCALHOLD_POSTGRES_MAX_CONNECTIONS",
        "LOCALHOLD_POSTGRES_AUTO_MIGRATE",
        "LOCALHOLD_EMBEDDING_BASE_URL",
        "LOCALHOLD_EMBEDDING_MODEL",
        "LOCALHOLD_EMBEDDING_API_KEY",
        "LOCALHOLD_EMBEDDING_AUTH_MODE",
        "LOCALHOLD_EMBEDDING_SEND_DIMENSIONS",
        "LOCALHOLD_EMBEDDING_HEALTH_CHECK",
        "LOCALHOLD_EMBEDDING_ALLOW_INSECURE_HTTP",
        "LOCALHOLD_EMBEDDING_DIMENSIONS",
        "LOCALHOLD_LOG_LEVEL",
        "LOCALHOLD_PRINCIPAL",
        "LOCALHOLD_ANONYMOUS_POLICY",
        "LOCALHOLD_TRANSPORT",
        "LOCALHOLD_HTTP_HOST",
        "LOCALHOLD_HTTP_PORT",
        "LOCALHOLD_HTTP_PATH",
        "LOCALHOLD_HTTP_AUTH_TOKEN",
        "LOCALHOLD_HTTP_PRINCIPAL_MODE",
        "LOCALHOLD_HTTP_PRINCIPAL",
        "LOCALHOLD_HTTP_PRINCIPAL_HEADER",
        "LOCALHOLD_HTTP_ALLOWED_HOSTS",
        "LOCALHOLD_HTTP_MAX_BODY_BYTES",
        "LOCALHOLD_HTTP_MAX_SESSIONS",
        "LOCALHOLD_HTTP_SESSION_IDLE_TIMEOUT_SECS",
        "LOCALHOLD_ADMIN_TOOLS_ENABLED",
        "LOCALHOLD_MAX_SEARCH_LIMIT",
        "LOCALHOLD_MAX_CANDIDATE_POOL_SIZE",
        "LOCALHOLD_MAX_LIST_LIMIT",
        "LOCALHOLD_MAX_CONTENT_LENGTH",
        "LOCALHOLD_MAX_TAGS_PER_MEMORY",
        "LOCALHOLD_MAX_TAG_LENGTH",
        "LOCALHOLD_MAX_BATCH_SIZE",
        "LOCALHOLD_MAX_REEMBED_LIMIT",
        "LOCALHOLD_EMBEDDING_TIMEOUT",
        "LOCALHOLD_MAX_CONCURRENT_EMBEDDING_REQUESTS",
        "LOCALHOLD_EMBEDDING_BATCH_SIZE",
        "LOCALHOLD_EMBEDDING_MAX_RETRIES",
        "LOCALHOLD_EMBEDDING_RETRY_INITIAL_BACKOFF_MS",
        "LOCALHOLD_EMBEDDING_RETRY_MAX_BACKOFF_MS",
        "LOCALHOLD_SHUTDOWN_TIMEOUT",
        "LOCALHOLD_MAX_TOP_TAGS_LIMIT",
        "LOCALHOLD_MAX_HISTORY_LIMIT",
        "LOCALHOLD_CONSOLIDATION_NEIGHBOR_LIMIT",
        "LOCALHOLD_MAX_ENTITIES_PER_MEMORY",
        "LOCALHOLD_MAX_ENTITY_FIELD_LENGTH",
        "LOCALHOLD_RRF_K",
        "LOCALHOLD_RRF_SEMANTIC_WEIGHT",
        "LOCALHOLD_RRF_KEYWORD_WEIGHT",
        "LOCALHOLD_DEFAULT_SEARCH_MODE",
        "LOCALHOLD_SEMANTIC_CANDIDATE_K",
        "LOCALHOLD_KEYWORD_CANDIDATE_K",
        "LOCALHOLD_RERANK_TOP_M",
        "LOCALHOLD_RERANKER_POOL_SIZE",
        "LOCALHOLD_RELEVANCE_WEIGHT",
        "LOCALHOLD_RECENCY_WEIGHT",
        "LOCALHOLD_IMPORTANCE_WEIGHT",
        "LOCALHOLD_RECENCY_HALF_LIFE_HOURS",
        "LOCALHOLD_ACTIVITY_WEIGHT",
        "LOCALHOLD_ACTIVITY_HALF_LIFE_HOURS",
        "LOCALHOLD_ACTIVITY_SATURATION",
        "LOCALHOLD_FRESHNESS_WEIGHT",
        "LOCALHOLD_FRESHNESS_HALF_LIFE_SEMANTIC_DAYS",
        "LOCALHOLD_FRESHNESS_HALF_LIFE_EPISODIC_DAYS",
        "LOCALHOLD_FRESHNESS_HALF_LIFE_PROCEDURAL_DAYS",
        "LOCALHOLD_CONFIDENCE_WEIGHT",
        "LOCALHOLD_RELEVANCE_FLOOR",
        "LOCALHOLD_RELEVANCE_FLOOR_PENALTY",
        "LOCALHOLD_SUPERSEDED_PENALTY",
        "LOCALHOLD_RERANKER_ENABLED",
        "LOCALHOLD_RERANKER_EXECUTION_PROVIDER",
        "LOCALHOLD_RERANKER_PRECISION",
        "LOCALHOLD_RERANKER_REQUIRED",
        "LOCALHOLD_RERANKER_MODEL",
        "LOCALHOLD_RERANKER_REVISION",
        "LOCALHOLD_RERANKER_MODEL_PATH",
        "LOCALHOLD_RERANKER_MODEL_SHA256",
        "LOCALHOLD_RERANKER_TOKENIZER_SHA256",
        "LOCALHOLD_RERANKER_BLEND_WEIGHT",
        "LOCALHOLD_RERANKER_CACHE_DIR",
        "LOCALHOLD_DUPLICATE_SUPPRESSION_ENABLED",
        "LOCALHOLD_DUPLICATE_SUPPRESSION_LAMBDA",
    ];
    keys.into_iter().filter_map(|key| std::env::var(key).ok().map(|v| (key.to_owned(), v))).collect()
}

#[cfg(test)]
mod tests;
