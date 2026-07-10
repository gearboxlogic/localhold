//! Configuration loading from TOML files and environment variable overrides.

use std::{
    collections::HashMap,
    fmt,
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{EngineError, ParseEnumError},
    types::SearchMode,
};

/// Default dimensionality used when embeddings are enabled.
pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 768;
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
    /// Anonymous callers may not read or write through the v2 facade.
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
    /// `HuggingFace` model identifier for the cross-encoder.
    pub model: String,
    /// Immutable `HuggingFace` revision/commit to download from.
    /// Required for custom auto-downloaded models.
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

/// Provider-agnostic embedding configuration.
///
/// Uses a tagged-union pattern so adding a new provider is a localized enum
/// variant rather than a change to shared struct fields.
///
/// # TOML layout
///
/// ```toml
/// [embedding]
/// provider = "openai_compatible"
/// dimensions = 768
///
/// [embedding.openai_compatible]
/// base_url = "http://127.0.0.1:8000/v1"
/// model = "nomic-embed-text"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EmbeddingConfig {
    /// OpenAI-compatible embedding provider.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible {
        /// Dimensionality of the embedding vectors.
        #[serde(default = "default_embedding_dimensions")]
        dimensions: usize,
        /// OpenAI-compatible endpoint parameters.
        #[serde(default)]
        openai_compatible: OpenAiCompatibleConfig,
    },
    /// No-op provider — memories are stored without embeddings.
    Noop {
        /// Dimensionality of the embedding vectors (used for schema initialization).
        #[serde(default = "default_embedding_dimensions")]
        dimensions: usize,
    },
}

/// Default dimensions helper for serde defaults.
const fn default_embedding_dimensions() -> usize {
    DEFAULT_EMBEDDING_DIMENSIONS
}

/// OpenAI-compatible embedding endpoint parameters.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct OpenAiCompatibleConfig {
    /// Base URL for the OpenAI-compatible API, including the `/v1` path.
    pub base_url: String,
    /// Embedding model name (e.g. `nomic-embed-text`).
    pub model: String,
    /// Optional bearer token. Local servers may ignore this, but some require it.
    pub api_key: Option<String>,
}

impl fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8000/v1".into(),
            model: "nomic-embed-text".into(),
            api_key: None,
        }
    }
}

impl EmbeddingConfig {
    /// Embedding vector dimensionality, regardless of provider.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        match self {
            Self::OpenAiCompatible { dimensions, .. } | Self::Noop { dimensions, .. } => *dimensions,
        }
    }

    /// OpenAI-compatible endpoint parameters, if using that provider.
    ///
    /// Returns `None` for non-OpenAI-compatible providers.
    #[must_use]
    pub const fn openai_compatible(&self) -> Option<&OpenAiCompatibleConfig> {
        match self {
            Self::OpenAiCompatible { openai_compatible, .. } => Some(openai_compatible),
            Self::Noop { .. } => None,
        }
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
    /// Maximum number of results for v2 `recall`.
    /// Default: 200 (balances result quality with response size; higher values
    /// increase latency from distance re-ranking).
    pub max_search_limit: usize,
    /// Maximum first-stage candidates fetched before reranking and composite scoring.
    /// Default: 1,000 (matches current vector backend safety ceiling).
    pub max_candidate_pool_size: usize,
    /// Maximum number of results for v2 `admin_list`.
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

#[expect(unused_must_use, reason = "best-effort stderr warning before tracing is ready")]
fn warn_env_parse(var: &str, value: &str) {
    writeln!(std::io::stderr(), "warning: ignoring unparseable {var}={value}");
}

/// Look up `var` in the env map and parse it into `target`. Logs a warning if the value is
/// present but unparseable.
fn apply_parsed_env<T: FromStr>(env: &HashMap<String, String>, var: &str, target: &mut T)
where
    T::Err: fmt::Display,
{
    if let Some(v) = env.get(var) {
        match v.parse() {
            Ok(parsed) => *target = parsed,
            Err(_) => warn_env_parse(var, v),
        }
    }
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
    /// Search order within the platform config directory is
    /// `localhold/localhold.toml`, then the legacy
    /// `localhold/recall.toml` compatibility path. Files in the current
    /// working directory are never loaded implicitly. Missing files are not
    /// an error — defaults apply.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Config` if a config file exists but cannot be read or parsed.
    pub fn load() -> Result<Self, EngineError> {
        let candidates = user_config_candidates(dirs::config_dir().as_deref());

        let env_map = collect_recall_env_vars();
        Self::load_from_sources(&candidates, &env_map)
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
        let mut config = None;
        for candidate in paths {
            if candidate.exists() {
                config = Some(Self::load_from_file(candidate)?);
                break;
            }
        }
        let mut config = config.unwrap_or_default();
        config.apply_env_from_map(env_map);
        config.resolve_paths()?;
        config.validate(env_map)?;
        Ok(config)
    }

    fn load_from_file(path: &Path) -> Result<Self, EngineError> {
        let contents = std::fs::read_to_string(path).map_err(|e| EngineError::config(format!("reading {}: {e}", path.display())))?;
        toml::from_str(&contents).map_err(|e| EngineError::config(format!("parsing {}: {e}", path.display())))
    }

    #[expect(clippy::too_many_lines, reason = "centralized config env override table is intentionally linear")]
    fn apply_env_from_map(&mut self, env: &HashMap<String, String>) {
        apply_parsed_env(env, "RECALL_DB_BACKEND", &mut self.database.backend);
        if let Some(v) = env.get("RECALL_DB_PATH") {
            self.database.path = Some(PathBuf::from(v));
        }
        if let Some(v) = env.get("RECALL_POSTGRES_URL") {
            self.database.postgres.url.clone_from(v);
        }
        apply_parsed_env(env, "RECALL_POSTGRES_MAX_CONNECTIONS", &mut self.database.postgres.max_connections);
        apply_parsed_env(env, "RECALL_POSTGRES_AUTO_MIGRATE", &mut self.database.postgres.auto_migrate);
        apply_embedding_env(&mut self.embedding, env);
        if let Some(v) = env.get("RECALL_LOG_LEVEL") {
            self.server.log_level.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_PRINCIPAL") {
            self.server.principal = Some(v.clone());
        }
        apply_parsed_env(env, "RECALL_ANONYMOUS_POLICY", &mut self.server.anonymous_policy);
        apply_parsed_env(env, "RECALL_TRANSPORT", &mut self.server.transport);
        if let Some(v) = env.get("RECALL_HTTP_HOST") {
            self.server.host.clone_from(v);
        }
        apply_parsed_env(env, "RECALL_HTTP_PORT", &mut self.server.port);
        if let Some(v) = env.get("RECALL_HTTP_PATH") {
            self.server.path.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_HTTP_AUTH_TOKEN") {
            self.server.http_auth_token = Some(v.clone());
        }
        apply_parsed_env(env, "RECALL_HTTP_PRINCIPAL_MODE", &mut self.server.http_principal_mode);
        if let Some(v) = env.get("RECALL_HTTP_PRINCIPAL") {
            self.server.http_principal.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_HTTP_PRINCIPAL_HEADER") {
            self.server.http_principal_header.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_HTTP_ALLOWED_HOSTS") {
            self.server.http_allowed_hosts = v.split(',').map(str::trim).filter(|host| !host.is_empty()).map(ToOwned::to_owned).collect();
        }
        apply_parsed_env(env, "RECALL_HTTP_MAX_BODY_BYTES", &mut self.server.max_body_bytes);
        apply_parsed_env(env, "RECALL_HTTP_MAX_SESSIONS", &mut self.server.http_max_sessions);
        apply_parsed_env(env, "RECALL_HTTP_SESSION_IDLE_TIMEOUT_SECS", &mut self.server.http_session_idle_timeout_secs);
        apply_parsed_env(env, "RECALL_ADMIN_TOOLS_ENABLED", &mut self.server.admin_tools_enabled);
        apply_parsed_env(env, "RECALL_MAX_SEARCH_LIMIT", &mut self.limits.max_search_limit);
        apply_parsed_env(env, "RECALL_MAX_CANDIDATE_POOL_SIZE", &mut self.limits.max_candidate_pool_size);
        apply_parsed_env(env, "RECALL_MAX_LIST_LIMIT", &mut self.limits.max_list_limit);
        apply_parsed_env(env, "RECALL_MAX_CONTENT_LENGTH", &mut self.limits.max_content_length);
        apply_parsed_env(env, "RECALL_MAX_TAGS_PER_MEMORY", &mut self.limits.max_tags_per_memory);
        apply_parsed_env(env, "RECALL_MAX_TAG_LENGTH", &mut self.limits.max_tag_length);
        apply_parsed_env(env, "RECALL_MAX_BATCH_SIZE", &mut self.limits.max_batch_size);
        apply_parsed_env(env, "RECALL_MAX_REEMBED_LIMIT", &mut self.limits.max_reembed_limit);
        apply_parsed_env(env, "RECALL_EMBEDDING_TIMEOUT", &mut self.limits.embedding_timeout_secs);
        apply_parsed_env(env, "RECALL_SHUTDOWN_TIMEOUT", &mut self.limits.shutdown_timeout_secs);
        apply_parsed_env(env, "RECALL_MAX_TOP_TAGS_LIMIT", &mut self.limits.max_top_tags_limit);
        apply_parsed_env(env, "RECALL_MAX_HISTORY_LIMIT", &mut self.limits.max_history_limit);
        apply_parsed_env(env, "RECALL_CONSOLIDATION_NEIGHBOR_LIMIT", &mut self.limits.consolidation_neighbor_limit);
        apply_parsed_env(env, "RECALL_MAX_ENTITIES_PER_MEMORY", &mut self.limits.max_entities_per_memory);
        apply_parsed_env(env, "RECALL_MAX_ENTITY_FIELD_LENGTH", &mut self.limits.max_entity_field_length);
        apply_parsed_env(env, "RECALL_RRF_K", &mut self.search.rrf_k);
        apply_parsed_env(env, "RECALL_RRF_SEMANTIC_WEIGHT", &mut self.search.rrf_semantic_weight);
        apply_parsed_env(env, "RECALL_RRF_KEYWORD_WEIGHT", &mut self.search.rrf_keyword_weight);
        apply_parsed_env(env, "RECALL_DEFAULT_SEARCH_MODE", &mut self.search.default_mode);
        apply_parsed_env(env, "RECALL_SEMANTIC_CANDIDATE_K", &mut self.search.semantic_candidate_k);
        apply_parsed_env(env, "RECALL_KEYWORD_CANDIDATE_K", &mut self.search.keyword_candidate_k);
        apply_parsed_env(env, "RECALL_RERANK_TOP_M", &mut self.search.rerank_top_m);
        apply_parsed_env(env, "RECALL_RERANKER_POOL_SIZE", &mut self.search.reranker.pool_size);
        apply_parsed_env(env, "RECALL_RELEVANCE_WEIGHT", &mut self.search.relevance_weight);
        // Backward compat: apply deprecated aliases first so the new name
        // takes precedence when both are set during migration.
        apply_parsed_env(env, "RECALL_RECENCY_WEIGHT", &mut self.search.activity_weight);
        apply_parsed_env(env, "RECALL_ACTIVITY_WEIGHT", &mut self.search.activity_weight);
        apply_parsed_env(env, "RECALL_IMPORTANCE_WEIGHT", &mut self.search.importance_weight);
        apply_parsed_env(env, "RECALL_RECENCY_HALF_LIFE_HOURS", &mut self.search.activity_half_life_hours);
        apply_parsed_env(env, "RECALL_ACTIVITY_HALF_LIFE_HOURS", &mut self.search.activity_half_life_hours);
        apply_parsed_env(env, "RECALL_ACTIVITY_SATURATION", &mut self.search.activity_saturation);
        apply_parsed_env(env, "RECALL_FRESHNESS_WEIGHT", &mut self.search.freshness_weight);
        apply_parsed_env(env, "RECALL_FRESHNESS_HALF_LIFE_SEMANTIC_DAYS", &mut self.search.freshness_half_life_semantic_days);
        apply_parsed_env(env, "RECALL_FRESHNESS_HALF_LIFE_EPISODIC_DAYS", &mut self.search.freshness_half_life_episodic_days);
        apply_parsed_env(env, "RECALL_FRESHNESS_HALF_LIFE_PROCEDURAL_DAYS", &mut self.search.freshness_half_life_procedural_days);
        apply_parsed_env(env, "RECALL_CONFIDENCE_WEIGHT", &mut self.search.confidence_weight);
        apply_parsed_env(env, "RECALL_RELEVANCE_FLOOR", &mut self.search.relevance_floor);
        apply_parsed_env(env, "RECALL_RELEVANCE_FLOOR_PENALTY", &mut self.search.relevance_floor_penalty);
        apply_parsed_env(env, "RECALL_SUPERSEDED_PENALTY", &mut self.search.superseded_penalty);
        apply_parsed_env(env, "RECALL_RERANKER_ENABLED", &mut self.search.reranker.enabled);
        if let Some(v) = env.get("RECALL_RERANKER_MODEL") {
            self.search.reranker.model.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_RERANKER_REVISION") {
            self.search.reranker.revision.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_RERANKER_MODEL_PATH") {
            self.search.reranker.model_path.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_RERANKER_MODEL_SHA256") {
            self.search.reranker.model_sha256.clone_from(v);
        }
        if let Some(v) = env.get("RECALL_RERANKER_TOKENIZER_SHA256") {
            self.search.reranker.tokenizer_sha256.clone_from(v);
        }
        apply_parsed_env(env, "RECALL_RERANKER_BLEND_WEIGHT", &mut self.search.reranker.blend_weight);
        if let Some(v) = env.get("RECALL_RERANKER_CACHE_DIR") {
            self.search.reranker.cache_dir.clone_from(v);
        }
        apply_parsed_env(env, "RECALL_DUPLICATE_SUPPRESSION_ENABLED", &mut self.search.duplicate_suppression.enabled);
        apply_parsed_env(env, "RECALL_DUPLICATE_SUPPRESSION_LAMBDA", &mut self.search.duplicate_suppression.lambda);
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
        if let EmbeddingConfig::OpenAiCompatible { openai_compatible, .. } = &mut self.embedding {
            openai_compatible.base_url = openai_compatible.base_url.trim().trim_end_matches('/').to_owned();
            openai_compatible.model = openai_compatible.model.trim().to_owned();
            openai_compatible.api_key = openai_compatible
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|api_key| !api_key.is_empty())
                .map(ToOwned::to_owned);
        }
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
        let rerank_top_m_explicit = self.search.rerank_top_m != SearchConfig::default().rerank_top_m || env.contains_key("RECALL_RERANK_TOP_M");
        if !rerank_top_m_explicit && self.search.reranker.pool_size != RerankerConfig::default().pool_size {
            self.search.rerank_top_m = self.search.reranker.pool_size;
        }
        validate_limits_config(&self.limits)?;
        validate_search_config(&self.search)?;
        Ok(())
    }
}

fn user_config_candidates(config_dir: Option<&Path>) -> Vec<PathBuf> {
    config_dir.map_or_else(Vec::new, |dir| {
        let localhold_dir = dir.join("localhold");
        vec![localhold_dir.join("localhold.toml"), localhold_dir.join("recall.toml")]
    })
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

/// Validate embedding configuration for the active provider.
fn validate_embedding_config(config: &EmbeddingConfig) -> Result<(), EngineError> {
    if config.dimensions() == 0 {
        return Err(EngineError::config("embedding dimensions must be greater than zero"));
    }

    if let EmbeddingConfig::OpenAiCompatible { openai_compatible, .. } = config {
        validate_openai_compatible_config(openai_compatible)?;
    }

    Ok(())
}

/// Validate OpenAI-compatible embedding endpoint fields.
fn validate_openai_compatible_config(config: &OpenAiCompatibleConfig) -> Result<(), EngineError> {
    let base_url = config.base_url.trim();
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(EngineError::config("embedding.openai_compatible.base_url must start with http:// or https://"));
    }
    let parsed = reqwest::Url::parse(base_url).map_err(|error| EngineError::config(format!("embedding.openai_compatible.base_url is invalid: {error}")))?;
    if parsed.host_str().is_none() {
        return Err(EngineError::config("embedding.openai_compatible.base_url must include a host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(EngineError::config(
            "embedding.openai_compatible.base_url must not contain credentials; use embedding.openai_compatible.api_key",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(EngineError::config("embedding.openai_compatible.base_url must not contain a query string or fragment"));
    }

    if config.model.trim().is_empty() {
        return Err(EngineError::config("embedding.openai_compatible.model must not be empty"));
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
    if config.reranker.pool_size == 0 {
        return Err(EngineError::config("reranker.pool_size must be greater than zero"));
    }
    if !config.reranker.blend_weight.is_finite() || !(0.0_f64..=1.0_f64).contains(&config.reranker.blend_weight) {
        return Err(EngineError::config("reranker.blend_weight must be a finite number in [0, 1]"));
    }
    // Validate auto-download requirements eagerly regardless of feature flag
    // so misconfigurations surface at startup, not at first search.
    if config.reranker.enabled && config.reranker.model_path.is_empty() {
        if is_builtin_default_reranker_model(&config.reranker.model) {
            // Overriding the revision on the builtin model requires explicit
            // hashes — the pinned defaults only match the pinned revision.
            let custom_revision = !config.reranker.revision.is_empty() && config.reranker.revision != DEFAULT_RERANKER_REVISION;
            if custom_revision && (config.reranker.model_sha256.is_empty() || config.reranker.tokenizer_sha256.is_empty()) {
                return Err(EngineError::config(
                    "overriding reranker.revision on the builtin model requires explicit model_sha256 and tokenizer_sha256",
                ));
            }
        } else {
            if config.reranker.revision.is_empty() {
                return Err(EngineError::config("reranker.revision must be set for custom auto-downloaded models"));
            }
            if config.reranker.model_sha256.is_empty() {
                return Err(EngineError::config("reranker.model_sha256 must be set for custom auto-downloaded models"));
            }
            if config.reranker.tokenizer_sha256.is_empty() {
                return Err(EngineError::config("reranker.tokenizer_sha256 must be set for custom auto-downloaded models"));
            }
        }
    }
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

/// Check if a model identifier matches the builtin default reranker model.
///
/// Accepts both the hyphenated (`L-6`) and canonical (`L6`) forms of the
/// model name to handle the `HuggingFace` model-card vs repo-name discrepancy.
pub(crate) fn is_builtin_default_reranker_model(model: &str) -> bool {
    model == DEFAULT_RERANKER_MODEL || model == DEFAULT_RERANKER_MODEL_CANONICAL
}

/// Apply embedding-related env overrides to an [`EmbeddingConfig`].
///
/// `RECALL_EMBEDDING_BASE_URL`, `RECALL_EMBEDDING_MODEL`, and
/// `RECALL_EMBEDDING_API_KEY` apply to the OpenAI-compatible provider.
/// `RECALL_EMBEDDING_DIMENSIONS` applies to whichever variant is active.
fn apply_embedding_env(config: &mut EmbeddingConfig, env: &HashMap<String, String>) {
    let selects_openai_compatible = ["RECALL_EMBEDDING_BASE_URL", "RECALL_EMBEDDING_MODEL", "RECALL_EMBEDDING_API_KEY"]
        .iter()
        .any(|key| env.contains_key(*key));
    if selects_openai_compatible && matches!(config, EmbeddingConfig::Noop { .. }) {
        let dimensions = config.dimensions();
        *config = EmbeddingConfig::OpenAiCompatible {
            dimensions,
            openai_compatible: OpenAiCompatibleConfig::default(),
        };
    }

    match config {
        EmbeddingConfig::OpenAiCompatible { dimensions, openai_compatible } => {
            if let Some(v) = env.get("RECALL_EMBEDDING_BASE_URL") {
                openai_compatible.base_url.clone_from(v);
            }
            if let Some(v) = env.get("RECALL_EMBEDDING_MODEL") {
                openai_compatible.model.clone_from(v);
            }
            if let Some(v) = env.get("RECALL_EMBEDDING_API_KEY") {
                openai_compatible.api_key = Some(v.clone());
            }
            apply_parsed_env(env, "RECALL_EMBEDDING_DIMENSIONS", dimensions);
        }
        EmbeddingConfig::Noop { dimensions } => {
            apply_parsed_env(env, "RECALL_EMBEDDING_DIMENSIONS", dimensions);
        }
    }
}

/// Collect all `RECALL_*` env vars into a map for [`Config::load_from_sources`].
#[expect(clippy::too_many_lines, reason = "explicit environment allowlist is intentionally centralized")]
fn collect_recall_env_vars() -> HashMap<String, String> {
    let keys = [
        "RECALL_DB_BACKEND",
        "RECALL_DB_PATH",
        "RECALL_POSTGRES_URL",
        "RECALL_POSTGRES_MAX_CONNECTIONS",
        "RECALL_POSTGRES_AUTO_MIGRATE",
        "RECALL_EMBEDDING_BASE_URL",
        "RECALL_EMBEDDING_MODEL",
        "RECALL_EMBEDDING_API_KEY",
        "RECALL_EMBEDDING_DIMENSIONS",
        "RECALL_LOG_LEVEL",
        "RECALL_PRINCIPAL",
        "RECALL_ANONYMOUS_POLICY",
        "RECALL_TRANSPORT",
        "RECALL_HTTP_HOST",
        "RECALL_HTTP_PORT",
        "RECALL_HTTP_PATH",
        "RECALL_HTTP_AUTH_TOKEN",
        "RECALL_HTTP_PRINCIPAL_MODE",
        "RECALL_HTTP_PRINCIPAL",
        "RECALL_HTTP_PRINCIPAL_HEADER",
        "RECALL_HTTP_ALLOWED_HOSTS",
        "RECALL_HTTP_MAX_BODY_BYTES",
        "RECALL_HTTP_MAX_SESSIONS",
        "RECALL_HTTP_SESSION_IDLE_TIMEOUT_SECS",
        "RECALL_ADMIN_TOOLS_ENABLED",
        "RECALL_MAX_SEARCH_LIMIT",
        "RECALL_MAX_CANDIDATE_POOL_SIZE",
        "RECALL_MAX_LIST_LIMIT",
        "RECALL_MAX_CONTENT_LENGTH",
        "RECALL_MAX_TAGS_PER_MEMORY",
        "RECALL_MAX_TAG_LENGTH",
        "RECALL_MAX_BATCH_SIZE",
        "RECALL_MAX_REEMBED_LIMIT",
        "RECALL_EMBEDDING_TIMEOUT",
        "RECALL_SHUTDOWN_TIMEOUT",
        "RECALL_MAX_TOP_TAGS_LIMIT",
        "RECALL_MAX_HISTORY_LIMIT",
        "RECALL_CONSOLIDATION_NEIGHBOR_LIMIT",
        "RECALL_MAX_ENTITIES_PER_MEMORY",
        "RECALL_MAX_ENTITY_FIELD_LENGTH",
        "RECALL_RRF_K",
        "RECALL_RRF_SEMANTIC_WEIGHT",
        "RECALL_RRF_KEYWORD_WEIGHT",
        "RECALL_DEFAULT_SEARCH_MODE",
        "RECALL_SEMANTIC_CANDIDATE_K",
        "RECALL_KEYWORD_CANDIDATE_K",
        "RECALL_RERANK_TOP_M",
        "RECALL_RERANKER_POOL_SIZE",
        "RECALL_RELEVANCE_WEIGHT",
        "RECALL_RECENCY_WEIGHT",
        "RECALL_IMPORTANCE_WEIGHT",
        "RECALL_RECENCY_HALF_LIFE_HOURS",
        "RECALL_ACTIVITY_WEIGHT",
        "RECALL_ACTIVITY_HALF_LIFE_HOURS",
        "RECALL_ACTIVITY_SATURATION",
        "RECALL_FRESHNESS_WEIGHT",
        "RECALL_FRESHNESS_HALF_LIFE_SEMANTIC_DAYS",
        "RECALL_FRESHNESS_HALF_LIFE_EPISODIC_DAYS",
        "RECALL_FRESHNESS_HALF_LIFE_PROCEDURAL_DAYS",
        "RECALL_CONFIDENCE_WEIGHT",
        "RECALL_RELEVANCE_FLOOR",
        "RECALL_RELEVANCE_FLOOR_PENALTY",
        "RECALL_SUPERSEDED_PENALTY",
        "RECALL_RERANKER_ENABLED",
        "RECALL_RERANKER_MODEL",
        "RECALL_RERANKER_REVISION",
        "RECALL_RERANKER_MODEL_PATH",
        "RECALL_RERANKER_MODEL_SHA256",
        "RECALL_RERANKER_TOKENIZER_SHA256",
        "RECALL_RERANKER_BLEND_WEIGHT",
        "RECALL_RERANKER_CACHE_DIR",
        "RECALL_DUPLICATE_SUPPRESSION_ENABLED",
        "RECALL_DUPLICATE_SUPPRESSION_LAMBDA",
    ];
    keys.into_iter().filter_map(|key| std::env::var(key).ok().map(|v| (key.to_owned(), v))).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("localhold-{name}-{}-{nanos}", std::process::id()))
    }

    fn no_env() -> HashMap<String, String> {
        HashMap::new()
    }

    fn env_with(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn default_config_has_sane_values() {
        let config = Config::default();
        assert!(config.embedding.openai_compatible().is_none());
        assert_eq!(config.embedding.dimensions(), 768);
        assert_eq!(config.server.transport, Transport::Stdio);
        assert_eq!(config.server.principal.as_deref(), Some("stdio"));
        assert_eq!(config.server.anonymous_policy, AnonymousPolicy::PublicReadOnly);
        assert_eq!(config.server.http_auth_token, None);
        assert_eq!(config.server.http_principal_mode, HttpPrincipalMode::Fixed);
        assert_eq!(config.server.http_principal, "http");
        assert_eq!(config.server.http_principal_header, DEFAULT_HTTP_PRINCIPAL_HEADER);
        assert_eq!(config.server.http_allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert_eq!(config.server.max_body_bytes, DEFAULT_HTTP_MAX_BODY_BYTES);
        assert_eq!(config.server.http_max_sessions, DEFAULT_HTTP_MAX_SESSIONS);
        assert_eq!(config.server.http_session_idle_timeout_secs, DEFAULT_HTTP_SESSION_IDLE_TIMEOUT_SECS);
        assert!(!config.server.admin_tools_enabled);
        assert_eq!(config.database.backend, DatabaseBackend::Sqlite);
        assert_eq!(config.database.path, None);
        assert!(config.database.sqlite_path().ends_with("localhold/localhold.db"));
        assert_eq!(config.database.postgres.url, "postgres://localhold:localhold@localhost:5432/localhold");
        assert_eq!(config.database.postgres.max_connections, 5);
        assert!(config.database.postgres.auto_migrate);
        assert_eq!(config.limits.max_search_limit, 50);
        assert_eq!(config.limits.max_candidate_pool_size, MAX_CANDIDATE_POOL_SIZE_CEILING);
        assert_eq!(config.limits.max_list_limit, 500);
        assert_eq!(config.limits.max_content_length, 65_536);
        assert_eq!(config.limits.max_tags_per_memory, 50);
        assert_eq!(config.limits.max_tag_length, 256);
        assert_eq!(config.limits.max_batch_size, 100);
        assert_eq!(config.limits.max_reembed_limit, 100);
        assert_eq!(config.limits.embedding_timeout_secs, 30);
        assert_eq!(config.limits.shutdown_timeout_secs, 10);
        assert_eq!(config.limits.max_top_tags_limit, 100);
        assert_eq!(config.limits.max_history_limit, 500);
        assert_eq!(config.limits.consolidation_neighbor_limit, 20);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert!(parsed.embedding.openai_compatible().is_none());
        assert_eq!(parsed.limits.max_search_limit, config.limits.max_search_limit);
        assert_eq!(parsed.limits.embedding_timeout_secs, config.limits.embedding_timeout_secs);
        assert_eq!(parsed.limits.max_history_limit, config.limits.max_history_limit);
    }

    #[test]
    fn debug_redacts_openai_api_key_but_keeps_endpoint_and_model() {
        let config = OpenAiCompatibleConfig {
            base_url: "https://embeddings.example/v1".into(),
            model: "useful-model-name".into(),
            api_key: Some("super-secret-api-key".into()),
        };

        let debug = format!("{config:?}");
        assert!(debug.contains("https://embeddings.example/v1"));
        assert!(debug.contains("useful-model-name"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret-api-key"));
    }

    #[test]
    fn debug_redacts_postgres_url_credentials_but_keeps_pool_settings() {
        let config = PostgresDatabaseConfig {
            url: "postgres://private-user:private-password@db.example/localhold".into(),
            max_connections: 17,
            auto_migrate: false,
        };

        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("max_connections: 17"));
        assert!(debug.contains("auto_migrate: false"));
        assert!(!debug.contains("private-user"));
        assert!(!debug.contains("private-password"));
        assert!(!debug.contains("db.example"));
    }

    #[test]
    fn limits_config_loadable_from_toml() {
        let toml_str = "[limits]\nmax_search_limit = 42\nmax_candidate_pool_size = 500\nembedding_timeout_secs = 60\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.limits.max_search_limit, 42);
        assert_eq!(config.limits.max_candidate_pool_size, 500);
        assert_eq!(config.limits.embedding_timeout_secs, 60);
        // Other limits retain defaults
        assert_eq!(config.limits.max_batch_size, 100);
    }

    #[test]
    fn env_overrides_apply() {
        let env = env_with(&[("RECALL_EMBEDDING_MODEL", "test-model")]);
        let mut config = Config::default();
        config.apply_env_from_map(&env);
        assert_eq!(config.embedding.openai_compatible().unwrap().model, "test-model");
    }

    #[test]
    fn env_overrides_keep_rerank_top_m_and_deprecated_pool_size_separate() {
        let env = env_with(&[("RECALL_RERANK_TOP_M", "25"), ("RECALL_RERANKER_POOL_SIZE", "40")]);
        let mut config = Config::default();
        config.apply_env_from_map(&env);
        config.validate(&std::collections::HashMap::new()).unwrap();

        assert_eq!(config.search.rerank_top_m, 25);
        assert_eq!(config.search.reranker.pool_size, 40);
    }

    #[test]
    fn env_overrides_apply_all_fields_and_ignore_unparseable_values() {
        let env = env_with(&[
            ("RECALL_DB_BACKEND", "postgres"),
            ("RECALL_DB_PATH", "/tmp/recall-test.db"),
            ("RECALL_POSTGRES_URL", "postgresql://recall:secret@localhost:5433/recall_test"),
            ("RECALL_POSTGRES_MAX_CONNECTIONS", "12"),
            ("RECALL_POSTGRES_AUTO_MIGRATE", "false"),
            ("RECALL_EMBEDDING_BASE_URL", "http://example.local/v1"),
            ("RECALL_EMBEDDING_MODEL", "embed-model"),
            ("RECALL_EMBEDDING_API_KEY", "embed-key"),
            ("RECALL_LOG_LEVEL", "debug"),
            ("RECALL_PRINCIPAL", "configured-agent"),
            ("RECALL_ANONYMOUS_POLICY", "deny_all"),
            ("RECALL_TRANSPORT", "http"),
            ("RECALL_HTTP_HOST", "0.0.0.0"),
            ("RECALL_HTTP_PORT", "invalid-http-port"),
            ("RECALL_HTTP_PATH", "/memory"),
            ("RECALL_HTTP_AUTH_TOKEN", "secret-token"),
            ("RECALL_HTTP_PRINCIPAL_MODE", "trusted_proxy"),
            ("RECALL_HTTP_PRINCIPAL", "proxy-fallback"),
            ("RECALL_HTTP_PRINCIPAL_HEADER", "X-Agent-Principal"),
            ("RECALL_HTTP_ALLOWED_HOSTS", "localhold.internal, 10.0.0.4:8080"),
            ("RECALL_HTTP_MAX_BODY_BYTES", "4096"),
            ("RECALL_HTTP_MAX_SESSIONS", "24"),
            ("RECALL_HTTP_SESSION_IDLE_TIMEOUT_SECS", "600"),
            ("RECALL_ADMIN_TOOLS_ENABLED", "true"),
        ]);

        let mut config = Config::default();
        let original_http_port = config.server.port;
        config.apply_env_from_map(&env);

        let embedding = config.embedding.openai_compatible().unwrap();
        assert_eq!(config.database.backend, DatabaseBackend::Postgres);
        assert_eq!(config.database.sqlite_path(), Path::new("/tmp/recall-test.db"));
        assert_eq!(config.database.postgres.url, "postgresql://recall:secret@localhost:5433/recall_test");
        assert_eq!(config.database.postgres.max_connections, 12);
        assert!(!config.database.postgres.auto_migrate);
        assert_eq!(embedding.base_url, "http://example.local/v1");
        assert_eq!(embedding.model, "embed-model");
        assert_eq!(embedding.api_key.as_deref(), Some("embed-key"));
        assert_eq!(config.server.log_level, "debug");
        assert_eq!(config.server.principal.as_deref(), Some("configured-agent"));
        assert_eq!(config.server.anonymous_policy, AnonymousPolicy::DenyAll);
        assert_eq!(config.server.transport, Transport::Http);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, original_http_port);
        assert_eq!(config.server.path, "/memory");
        assert_eq!(config.server.http_auth_token.as_deref(), Some("secret-token"));
        assert_eq!(config.server.http_principal_mode, HttpPrincipalMode::TrustedProxy);
        assert_eq!(config.server.http_principal, "proxy-fallback");
        assert_eq!(config.server.http_principal_header, "X-Agent-Principal");
        assert_eq!(config.server.http_allowed_hosts, ["localhold.internal", "10.0.0.4:8080"]);
        assert_eq!(config.server.max_body_bytes, 4096);
        assert_eq!(config.server.http_max_sessions, 24);
        assert_eq!(config.server.http_session_idle_timeout_secs, 600);
        assert!(config.server.admin_tools_enabled);
    }

    #[test]
    fn env_overrides_apply_limits() {
        let env = env_with(&[
            ("RECALL_MAX_SEARCH_LIMIT", "42"),
            ("RECALL_MAX_CANDIDATE_POOL_SIZE", "500"),
            ("RECALL_MAX_LIST_LIMIT", "1000"),
            ("RECALL_MAX_CONTENT_LENGTH", "131072"),
            ("RECALL_MAX_TAGS_PER_MEMORY", "25"),
            ("RECALL_MAX_TAG_LENGTH", "128"),
            ("RECALL_MAX_BATCH_SIZE", "50"),
            ("RECALL_MAX_REEMBED_LIMIT", "75"),
            ("RECALL_EMBEDDING_TIMEOUT", "60"),
            ("RECALL_SHUTDOWN_TIMEOUT", "15"),
            ("RECALL_MAX_TOP_TAGS_LIMIT", "50"),
            ("RECALL_MAX_HISTORY_LIMIT", "250"),
        ]);

        let mut config = Config::default();
        config.apply_env_from_map(&env);

        assert_eq!(config.limits.max_search_limit, 42);
        assert_eq!(config.limits.max_candidate_pool_size, 500);
        assert_eq!(config.limits.max_list_limit, 1000);
        assert_eq!(config.limits.max_content_length, 131_072);
        assert_eq!(config.limits.max_tags_per_memory, 25);
        assert_eq!(config.limits.max_tag_length, 128);
        assert_eq!(config.limits.max_batch_size, 50);
        assert_eq!(config.limits.max_reembed_limit, 75);
        assert_eq!(config.limits.embedding_timeout_secs, 60);
        assert_eq!(config.limits.shutdown_timeout_secs, 15);
        assert_eq!(config.limits.max_top_tags_limit, 50);
        assert_eq!(config.limits.max_history_limit, 250);
    }

    #[test]
    fn default_config_has_http_defaults() {
        let config = Config::default();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.path, "/mcp");
        assert_eq!(config.server.http_allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert_eq!(config.server.max_body_bytes, DEFAULT_HTTP_MAX_BODY_BYTES);
    }

    #[test]
    fn serde_rejects_unknown_transport() {
        let toml_str = "[server]\ntransport = \"websocket\"\n";
        let result: Result<Config, _> = toml::from_str(toml_str);
        let _err = result.unwrap_err();
    }

    #[test]
    fn serde_accepts_known_transports() {
        for (name, expected) in [("stdio", Transport::Stdio), ("http", Transport::Http)] {
            let toml_str = format!("[server]\ntransport = \"{name}\"\n");
            let config: Config = toml::from_str(&toml_str).unwrap();
            assert_eq!(config.server.transport, expected);
        }
    }

    #[test]
    fn serde_rejects_unknown_database_backend() {
        let toml_str = "[database]\nbackend = \"mysql\"\n";
        let result: Result<Config, _> = toml::from_str(toml_str);
        let _err = result.unwrap_err();
    }

    #[test]
    fn serde_accepts_known_database_backends() {
        for (name, expected) in [("sqlite", DatabaseBackend::Sqlite), ("postgres", DatabaseBackend::Postgres)] {
            let toml_str = format!("[database]\nbackend = \"{name}\"\n");
            let config: Config = toml::from_str(&toml_str).unwrap();
            assert_eq!(config.database.backend, expected);
        }
    }

    #[test]
    fn database_config_sqlite_from_toml() {
        let toml_str = "[database]\nbackend = \"sqlite\"\n\n[database.sqlite]\npath = \"/tmp/custom-recall.db\"\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.database.backend, DatabaseBackend::Sqlite);
        assert_eq!(config.database.sqlite_path(), Path::new("/tmp/custom-recall.db"));
    }

    #[test]
    fn database_config_legacy_path_alias_from_toml() {
        let toml_str = "[database]\npath = \"/tmp/legacy-recall.db\"\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.database.backend, DatabaseBackend::Sqlite);
        assert_eq!(config.database.sqlite_path(), Path::new("/tmp/legacy-recall.db"));
    }

    #[test]
    fn database_config_postgres_from_toml() {
        let toml_str = "[database]\nbackend = \"postgres\"\n\n[database.postgres]\nurl = \"postgresql://recall:secret@localhost:5433/recall_test\"\nmax_connections = 9\nauto_migrate = false\n";
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.validate(&no_env()).unwrap();
        assert_eq!(config.database.backend, DatabaseBackend::Postgres);
        assert_eq!(config.database.postgres.url, "postgresql://recall:secret@localhost:5433/recall_test");
        assert_eq!(config.database.postgres.max_connections, 9);
        assert!(!config.database.postgres.auto_migrate);
    }

    #[test]
    fn embedding_config_openai_compatible_from_toml() {
        let toml_str = "[embedding]\nprovider = \"openai_compatible\"\ndimensions = 384\n\n[embedding.openai_compatible]\nbase_url = \"http://remote:9999/v1\"\nmodel = \"custom-model\"\napi_key = \"secret\"\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.embedding.dimensions(), 384);
        let embedding = config.embedding.openai_compatible().unwrap();
        assert_eq!(embedding.base_url, "http://remote:9999/v1");
        assert_eq!(embedding.model, "custom-model");
        assert_eq!(embedding.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn embedding_config_noop_from_toml() {
        let toml_str = "[embedding]\nprovider = \"noop\"\ndimensions = 512\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.embedding.dimensions(), 512);
        assert!(config.embedding.openai_compatible().is_none(), "noop has no OpenAI-compatible config");
    }

    #[test]
    fn embedding_config_rejects_unknown_provider() {
        let toml_str = "[embedding]\nprovider = \"ollama\"\n";
        let result: Result<Config, _> = toml::from_str(toml_str);
        let _err = result.unwrap_err();
    }

    #[test]
    fn tilde_expansion() {
        let mut config = Config::default();
        config.database.path = Some(PathBuf::from("~/data/legacy-recall.db"));
        config.database.sqlite.path = PathBuf::from("~/data/recall.db");
        config.resolve_paths().unwrap();
        assert!(!config.database.path.as_ref().unwrap().to_str().unwrap().starts_with("~/"));
        assert!(!config.database.sqlite.path.to_str().unwrap().starts_with("~/"));
    }

    #[test]
    fn validate_database_config_rejects_bad_active_postgres_url() {
        let mut config = DatabaseConfig {
            backend: DatabaseBackend::Postgres,
            postgres: PostgresDatabaseConfig {
                url: "http://localhost:5432/recall".into(),
                ..PostgresDatabaseConfig::default()
            },
            ..DatabaseConfig::default()
        };
        let err = validate_database_config(&mut config).unwrap_err();
        assert!(err.to_string().contains("database.postgres.url"));
    }

    #[test]
    fn validate_database_config_rejects_zero_postgres_connections() {
        let mut config = DatabaseConfig {
            backend: DatabaseBackend::Postgres,
            postgres: PostgresDatabaseConfig {
                max_connections: 0,
                ..PostgresDatabaseConfig::default()
            },
            ..DatabaseConfig::default()
        };
        let err = validate_database_config(&mut config).unwrap_err();
        assert!(err.to_string().contains("database.postgres.max_connections"));
    }

    #[test]
    fn validate_trims_model_name() {
        let mut config = Config {
            embedding: EmbeddingConfig::OpenAiCompatible {
                dimensions: DEFAULT_EMBEDDING_DIMENSIONS,
                openai_compatible: OpenAiCompatibleConfig {
                    base_url: "  http://127.0.0.1:8000/v1/  ".into(),
                    model: "  nomic-embed-text  ".into(),
                    api_key: Some("  token  ".into()),
                },
            },
            ..Config::default()
        };
        config.validate(&std::collections::HashMap::new()).unwrap();
        let embedding = config.embedding.openai_compatible().unwrap();
        assert_eq!(embedding.base_url, "http://127.0.0.1:8000/v1");
        assert_eq!(embedding.model, "nomic-embed-text");
        assert_eq!(embedding.api_key.as_deref(), Some("token"));
    }

    #[test]
    fn user_config_candidates_prefer_canonical_file_over_legacy_file() {
        let root = unique_temp_dir("config-sources-precedence");
        let localhold_dir = root.join("localhold");
        fs::create_dir_all(&localhold_dir).unwrap();

        fs::write(
            localhold_dir.join("localhold.toml"),
            "[embedding]\nprovider = \"openai_compatible\"\n\n[embedding.openai_compatible]\nmodel = \"canonical\"\n",
        )
        .unwrap();
        fs::write(
            localhold_dir.join("recall.toml"),
            "[embedding]\nprovider = \"openai_compatible\"\n\n[embedding.openai_compatible]\nmodel = \"legacy\"\n",
        )
        .unwrap();

        let paths = user_config_candidates(Some(&root));
        let config = Config::load_from_sources(&paths, &no_env()).unwrap();
        assert_eq!(config.embedding.openai_compatible().unwrap().model, "canonical");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_config_candidates_fall_back_to_legacy_file() {
        let root = unique_temp_dir("config-sources-legacy");
        let localhold_dir = root.join("localhold");
        fs::create_dir_all(&localhold_dir).unwrap();
        fs::write(
            localhold_dir.join("recall.toml"),
            "[embedding]\nprovider = \"openai_compatible\"\n\n[embedding.openai_compatible]\nmodel = \"legacy\"\n",
        )
        .unwrap();

        let paths = user_config_candidates(Some(&root));
        let config = Config::load_from_sources(&paths, &no_env()).unwrap();
        assert_eq!(config.embedding.openai_compatible().unwrap().model, "legacy");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_config_candidates_never_include_current_directory_files() {
        let root = unique_temp_dir("config-sources-no-cwd");
        let paths = user_config_candidates(Some(&root));

        assert_eq!(paths, [root.join("localhold/localhold.toml"), root.join("localhold/recall.toml")]);
        assert!(paths.iter().all(|path| path.is_absolute()));
        assert!(!paths.iter().any(|path| path == Path::new("localhold.toml") || path == Path::new("recall.toml")));
    }

    #[test]
    fn load_from_sources_uses_defaults_when_no_paths_exist() {
        let paths: Vec<PathBuf> = vec![PathBuf::from("/nonexistent/localhold.toml")];
        let config = Config::load_from_sources(&paths, &no_env()).unwrap();
        assert!(config.embedding.openai_compatible().is_none());
    }

    #[test]
    fn load_from_sources_applies_env_overrides() {
        let env = env_with(&[
            ("RECALL_EMBEDDING_MODEL", "env-model"),
            ("RECALL_MAX_BATCH_SIZE", "25"),
            ("RECALL_PRINCIPAL", " "),
            ("RECALL_HTTP_AUTH_TOKEN", "  "),
            ("RECALL_HTTP_PRINCIPAL_HEADER", " X-LocalHold-Principal "),
        ]);
        let config = Config::load_from_sources(&[], &env).unwrap();
        assert_eq!(config.embedding.openai_compatible().unwrap().model, "env-model");
        assert_eq!(config.limits.max_batch_size, 25);
        assert_eq!(config.server.principal, None);
        assert_eq!(config.server.http_auth_token, None);
        assert_eq!(config.server.http_principal_header, "x-localhold-principal");
    }

    #[test]
    fn load_from_sources_rejects_invalid_http_principal_header() {
        let env = env_with(&[("RECALL_HTTP_PRINCIPAL_HEADER", "bad header")]);
        let err = Config::load_from_sources(&[], &env).unwrap_err();
        assert!(err.to_string().contains("server.http_principal_header"));
    }

    #[test]
    fn load_from_sources_env_overrides_file_values() {
        let root = unique_temp_dir("config-sources-env-override");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("localhold.toml"),
            "[embedding]\nprovider = \"openai_compatible\"\n\n[embedding.openai_compatible]\nmodel = \"from-file\"\n",
        )
        .unwrap();

        let paths = vec![root.join("localhold.toml")];
        let env = env_with(&[("RECALL_EMBEDDING_MODEL", "from-env")]);
        let config = Config::load_from_sources(&paths, &env).unwrap();
        assert_eq!(config.embedding.openai_compatible().unwrap().model, "from-env");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_from_sources_returns_parse_error_for_malformed_file() {
        let root = unique_temp_dir("config-sources-parse-error");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("localhold.toml"), "embedding = [").unwrap();

        let paths = vec![root.join("localhold.toml")];
        let err = Config::load_from_sources(&paths, &no_env()).unwrap_err();
        assert!(matches!(err, EngineError::Config(_)));
        let msg = err.to_string();
        assert!(msg.contains("parsing"));
        assert!(msg.contains("localhold.toml"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_from_sources_returns_read_error_when_path_is_directory() {
        let root = unique_temp_dir("config-sources-read-error");
        fs::create_dir_all(root.join("localhold.toml")).unwrap();

        let paths = vec![root.join("localhold.toml")];
        let err = Config::load_from_sources(&paths, &no_env()).unwrap_err();
        assert!(matches!(err, EngineError::Config(_)));
        let msg = err.to_string();
        assert!(msg.contains("reading"));
        assert!(msg.contains("localhold.toml"));

        fs::remove_dir_all(root).unwrap();
    }

    fn assert_zero_limit_rejected<F>(field: &str, mutate: F)
    where
        F: FnOnce(&mut LimitsConfig),
    {
        let mut limits = LimitsConfig::default();
        mutate(&mut limits);
        let err = validate_limits_config(&limits).unwrap_err();
        assert!(err.to_string().contains(field), "error should mention {field}: {err}");
    }

    #[test]
    fn validate_limits_config_rejects_zero_operational_limits() {
        assert_zero_limit_rejected("max_search_limit", |limits| limits.max_search_limit = 0);
        assert_zero_limit_rejected("max_candidate_pool_size", |limits| limits.max_candidate_pool_size = 0);
        assert_zero_limit_rejected("max_list_limit", |limits| limits.max_list_limit = 0);
        assert_zero_limit_rejected("max_content_length", |limits| limits.max_content_length = 0);
        assert_zero_limit_rejected("max_tags_per_memory", |limits| limits.max_tags_per_memory = 0);
        assert_zero_limit_rejected("max_tag_length", |limits| limits.max_tag_length = 0);
        assert_zero_limit_rejected("max_batch_size", |limits| limits.max_batch_size = 0);
        assert_zero_limit_rejected("max_reembed_limit", |limits| limits.max_reembed_limit = 0);
        assert_zero_limit_rejected("embedding_timeout_secs", |limits| limits.embedding_timeout_secs = 0);
        assert_zero_limit_rejected("shutdown_timeout_secs", |limits| limits.shutdown_timeout_secs = 0);
        assert_zero_limit_rejected("max_top_tags_limit", |limits| limits.max_top_tags_limit = 0);
        assert_zero_limit_rejected("max_history_limit", |limits| limits.max_history_limit = 0);
        assert_zero_limit_rejected("max_entities_per_memory", |limits| limits.max_entities_per_memory = 0);
        assert_zero_limit_rejected("max_entity_field_length", |limits| limits.max_entity_field_length = 0);
    }

    #[test]
    fn validate_server_config_rejects_zero_http_body_limit() {
        let config = ServerConfig {
            max_body_bytes: 0,
            ..ServerConfig::default()
        };
        let err = validate_server_config(&config).unwrap_err();
        assert!(err.to_string().contains("max_body_bytes"));
    }

    #[test]
    fn validate_server_config_rejects_zero_http_session_limit() {
        let config = ServerConfig {
            http_max_sessions: 0,
            ..ServerConfig::default()
        };
        let err = validate_server_config(&config).unwrap_err();
        assert!(err.to_string().contains("http_max_sessions"));
    }

    #[test]
    fn validate_server_config_rejects_zero_http_session_idle_timeout() {
        let config = ServerConfig {
            http_session_idle_timeout_secs: 0,
            ..ServerConfig::default()
        };
        let err = validate_server_config(&config).unwrap_err();
        assert!(err.to_string().contains("http_session_idle_timeout_secs"));
    }

    #[test]
    fn validate_server_config_accepts_static_http_paths() {
        for path in ["/", "/mcp", "/mcp/v1", "/mcp-v1", "/%6dcp"] {
            let config = ServerConfig {
                path: path.to_owned(),
                ..ServerConfig::default()
            };
            validate_server_config(&config).unwrap();
        }
    }

    #[test]
    fn validate_server_config_rejects_invalid_http_paths() {
        for path in [
            "",
            "mcp",
            "/mcp/",
            "//mcp",
            "/mcp//v1",
            "/m cp",
            "/mcp?debug=1",
            "/mcp#fragment",
            "/{mcp}",
            "/*mcp",
            "/:mcp",
        ] {
            let config = ServerConfig {
                path: path.to_owned(),
                ..ServerConfig::default()
            };
            let error = validate_server_config(&config).unwrap_err();
            assert!(error.to_string().contains("server.path"), "unexpected error for {path:?}: {error}");
        }
    }

    #[test]
    fn validate_server_config_rejects_invalid_http_allowed_hosts() {
        for hosts in [vec![], vec!["*".to_owned()], vec!["https://localhold.internal".to_owned()], vec!["bad host".to_owned()]] {
            let config = ServerConfig {
                http_allowed_hosts: hosts,
                ..ServerConfig::default()
            };
            let error = validate_server_config(&config).unwrap_err();
            assert!(error.to_string().contains("http_allowed_hosts"), "unexpected error: {error}");
        }
    }

    #[test]
    fn validate_server_config_rejects_invalid_http_auth_tokens() {
        for token in ["", "token with spaces", "token\nwith-newline", "t\u{f6}ken"] {
            let config = ServerConfig {
                http_auth_token: Some(token.to_owned()),
                ..ServerConfig::default()
            };
            let error = validate_server_config(&config).unwrap_err();
            assert!(error.to_string().contains("http_auth_token"), "unexpected error: {error}");
        }
    }

    #[test]
    fn validate_server_config_rejects_empty_fixed_http_principal() {
        let config = ServerConfig {
            http_principal: String::new(),
            ..ServerConfig::default()
        };
        let error = validate_server_config(&config).unwrap_err();
        assert!(error.to_string().contains("http_principal"));
    }

    #[test]
    fn validate_server_config_requires_auth_for_http_trusted_proxy_mode() {
        let config = ServerConfig {
            transport: Transport::Http,
            http_principal_mode: HttpPrincipalMode::TrustedProxy,
            http_auth_token: None,
            ..ServerConfig::default()
        };
        let error = validate_server_config(&config).unwrap_err();
        assert!(error.to_string().contains("http_auth_token"));
    }

    #[test]
    fn validate_limits_config_rejects_candidate_pool_above_backend_ceiling() {
        let limits = LimitsConfig {
            max_candidate_pool_size: MAX_CANDIDATE_POOL_SIZE_CEILING.saturating_add(1),
            ..LimitsConfig::default()
        };
        let err = validate_limits_config(&limits).unwrap_err();
        assert!(err.to_string().contains("max_candidate_pool_size"));
    }

    #[test]
    fn validate_limits_config_rejects_candidate_pool_below_search_limit() {
        let limits = LimitsConfig {
            max_search_limit: 50,
            max_candidate_pool_size: 49,
            ..LimitsConfig::default()
        };
        let err = validate_limits_config(&limits).unwrap_err();
        assert!(err.to_string().contains("max_candidate_pool_size"));
        assert!(err.to_string().contains("max_search_limit"));
    }

    #[test]
    fn validate_rejects_subnormal_activity_half_life() {
        let config = SearchConfig {
            activity_half_life_hours: 0.0001_f64,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("activity_half_life_hours"), "error should mention the field name");
        assert!(msg.contains("0.001"), "error should mention the minimum threshold");
    }

    #[test]
    fn validate_accepts_minimum_activity_half_life() {
        let config = SearchConfig {
            activity_half_life_hours: 0.001_f64,
            ..SearchConfig::default()
        };
        validate_search_config(&config).unwrap();
    }

    // -- RR-013: validate_embedding_config / validate_openai_compatible_config -----------

    #[test]
    fn validate_embedding_config_zero_dimensions_rejected() {
        let config = EmbeddingConfig::Noop { dimensions: 0 };
        let err = validate_embedding_config(&config).unwrap_err();
        assert!(err.to_string().contains("dimensions"), "error should mention dimensions");
    }

    #[test]
    fn validate_openai_compatible_config_url_without_http_prefix() {
        let config = OpenAiCompatibleConfig {
            base_url: "localhost".into(),
            ..OpenAiCompatibleConfig::default()
        };
        let err = validate_openai_compatible_config(&config).unwrap_err();
        assert!(err.to_string().contains("http"), "error should mention http prefix");
    }

    #[test]
    fn validate_openai_compatible_config_url_with_port_and_path() {
        let config = OpenAiCompatibleConfig {
            base_url: "http://localhost:11434/v1".into(),
            ..OpenAiCompatibleConfig::default()
        };
        validate_openai_compatible_config(&config).unwrap();
    }

    #[test]
    fn validate_openai_compatible_config_rejects_url_credentials() {
        let config = OpenAiCompatibleConfig {
            base_url: "https://user:secret@example.com/v1".into(),
            ..OpenAiCompatibleConfig::default()
        };
        let err = validate_openai_compatible_config(&config).unwrap_err();
        assert!(err.to_string().contains("must not contain credentials"));
    }

    #[test]
    fn validate_openai_compatible_config_rejects_query_and_fragment() {
        for base_url in ["https://example.com/v1?token=secret", "https://example.com/v1#secret"] {
            let config = OpenAiCompatibleConfig {
                base_url: base_url.into(),
                ..OpenAiCompatibleConfig::default()
            };
            let err = validate_openai_compatible_config(&config).unwrap_err();
            assert!(err.to_string().contains("query string or fragment"));
        }
    }

    #[test]
    fn validate_openai_compatible_config_empty_model_name() {
        let config = OpenAiCompatibleConfig {
            model: "   ".into(),
            ..OpenAiCompatibleConfig::default()
        };
        let err = validate_openai_compatible_config(&config).unwrap_err();
        assert!(err.to_string().contains("model"), "error should mention model name");
    }

    #[test]
    fn validate_openai_compatible_config_valid() {
        let config = OpenAiCompatibleConfig::default();
        validate_openai_compatible_config(&config).unwrap();
    }

    // -- RR-041: validate_search_config --------------------------------------

    #[test]
    fn validate_search_config_rrf_k_zero_rejected() {
        let config = SearchConfig {
            rrf_k: 0,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("rrf_k"), "error should mention rrf_k");
    }

    #[test]
    fn validate_search_config_nan_weight_rejected() {
        let config = SearchConfig {
            relevance_weight: f64::NAN,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("relevance_weight"), "error should mention the weight field");
    }

    #[test]
    fn validate_search_config_negative_weight_rejected() {
        let config = SearchConfig {
            activity_weight: -0.1_f64,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("activity_weight"), "error should mention the weight field");
    }

    #[test]
    fn validate_search_config_zero_activity_half_life_rejected() {
        let config = SearchConfig {
            activity_half_life_hours: 0.0,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("activity_half_life_hours"), "error should mention half life");
    }

    #[test]
    fn validate_search_config_valid() {
        let config = SearchConfig::default();
        validate_search_config(&config).unwrap();
    }

    #[test]
    fn validate_search_config_builtin_default_reranker_allows_embedded_pins() {
        let config = SearchConfig {
            reranker: RerankerConfig {
                enabled: true,
                model: DEFAULT_RERANKER_MODEL_CANONICAL.into(),
                ..RerankerConfig::default()
            },
            ..SearchConfig::default()
        };
        validate_search_config(&config).unwrap();
    }

    #[test]
    fn validate_search_config_custom_reranker_requires_revision_and_hashes() {
        let config = SearchConfig {
            reranker: RerankerConfig {
                enabled: true,
                model: "custom/reranker".into(),
                ..RerankerConfig::default()
            },
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("reranker.revision"));
    }

    #[test]
    fn validate_search_config_weights_must_sum_to_100() {
        let config = SearchConfig {
            relevance_weight: 50.0,
            importance_weight: 10.0,
            freshness_weight: 5.0,
            activity_weight: 5.0,
            confidence_weight: 5.0,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("sum to 100.0"), "error should mention weight sum: {err}");
    }

    #[test]
    fn validate_search_config_weights_over_100_rejected() {
        let config = SearchConfig {
            relevance_weight: 60.0,
            importance_weight: 30.0,
            freshness_weight: 10.0,
            activity_weight: 10.0,
            confidence_weight: 5.0,
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("sum to 100.0"), "error should mention weight sum: {err}");
    }

    // -- RR-062 (partial): enum FromStr error paths for config enums ---------

    #[test]
    fn transport_from_str_unknown_rejected() {
        let result = "unknown".parse::<Transport>();
        assert!(result.is_err(), "unknown transport should be rejected");
    }

    #[test]
    fn search_mode_from_str_unknown_rejected() {
        let result = "unknown".parse::<SearchMode>();
        assert!(result.is_err(), "unknown search mode should be rejected");
    }

    // -- RR-126: validate_positive_finite boundary tests ---------------------

    #[test]
    fn validate_positive_finite_negative_rejected() {
        let err = validate_positive_finite(-1.0_f64, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
    }

    #[test]
    fn validate_positive_finite_zero_rejected() {
        let err = validate_positive_finite(0.0_f64, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
        assert!(err.to_string().contains("positive"), "error should mention 'positive'");
    }

    #[test]
    fn validate_positive_finite_nan_rejected() {
        let err = validate_positive_finite(f64::NAN, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
    }

    #[test]
    fn validate_positive_finite_positive_infinity_rejected() {
        let err = validate_positive_finite(f64::INFINITY, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
    }

    #[test]
    fn validate_positive_finite_negative_infinity_rejected() {
        let err = validate_positive_finite(f64::NEG_INFINITY, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
    }

    #[test]
    fn validate_positive_finite_small_positive_accepted() {
        validate_positive_finite(0.001_f64, "test_field").unwrap();
    }

    #[test]
    fn validate_positive_finite_large_value_accepted() {
        validate_positive_finite(1e10_f64, "test_field").unwrap();
    }

    #[test]
    fn validate_positive_finite_one_accepted() {
        validate_positive_finite(1.0_f64, "test_field").unwrap();
    }

    // -- validate_non_negative_finite boundary tests (complementary) ---------

    #[test]
    fn validate_non_negative_finite_zero_accepted() {
        validate_non_negative_finite(0.0_f64, "test_field").unwrap();
    }

    #[test]
    fn validate_non_negative_finite_negative_rejected() {
        let err = validate_non_negative_finite(-0.1_f64, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
    }

    #[test]
    fn validate_non_negative_finite_nan_rejected() {
        let err = validate_non_negative_finite(f64::NAN, "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"), "error should mention field name");
    }

    #[test]
    fn validate_non_negative_finite_positive_accepted() {
        validate_non_negative_finite(42.0_f64, "test_field").unwrap();
    }
}
