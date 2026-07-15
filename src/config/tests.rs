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
    assert_eq!(config.database.postgres.migration_lock_timeout_secs, 5);
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
    assert_eq!(config.limits.max_concurrent_embedding_requests, 8);
    assert_eq!(config.limits.embedding_batch_size, 32);
    assert_eq!(config.limits.embedding_max_retries, 2);
    assert_eq!(config.limits.embedding_retry_initial_backoff_ms, 500);
    assert_eq!(config.limits.embedding_retry_max_backoff_ms, 30_000);
    assert_eq!(config.limits.shutdown_timeout_secs, 10);
    assert_eq!(config.limits.max_top_tags_limit, 100);
    assert_eq!(config.limits.max_history_limit, 500);
    assert_eq!(config.limits.consolidation_neighbor_limit, 20);
    assert!(!config.search.reranker.enabled);
    assert_eq!(config.search.reranker.execution_provider, RerankerExecutionProvider::Auto);
    assert_eq!(config.search.reranker.precision, RerankerPrecision::Fp32);
    assert!(!config.search.reranker.required);
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
        ..OpenAiCompatibleConfig::default()
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
        migration_lock_timeout_secs: 23,
        auto_migrate: false,
    };

    let debug = format!("{config:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(debug.contains("max_connections: 17"));
    assert!(debug.contains("migration_lock_timeout_secs: 23"));
    assert!(debug.contains("auto_migrate: false"));
    assert!(!debug.contains("private-user"));
    assert!(!debug.contains("private-password"));
    assert!(!debug.contains("db.example"));
}

#[test]
fn limits_config_loadable_from_toml() {
    let toml_str = "[limits]\nmax_search_limit = 42\nmax_candidate_pool_size = 500\nembedding_timeout_secs = 60\nmax_concurrent_embedding_requests = 4\nembedding_batch_size = 16\nembedding_max_retries = 5\nembedding_retry_initial_backoff_ms = 750\nembedding_retry_max_backoff_ms = 45000\n";
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.limits.max_search_limit, 42);
    assert_eq!(config.limits.max_candidate_pool_size, 500);
    assert_eq!(config.limits.embedding_timeout_secs, 60);
    assert_eq!(config.limits.max_concurrent_embedding_requests, 4);
    assert_eq!(config.limits.embedding_batch_size, 16);
    assert_eq!(config.limits.embedding_max_retries, 5);
    assert_eq!(config.limits.embedding_retry_initial_backoff_ms, 750);
    assert_eq!(config.limits.embedding_retry_max_backoff_ms, 45_000);
    // Other limits retain defaults
    assert_eq!(config.limits.max_batch_size, 100);
}

#[test]
fn env_overrides_apply() {
    let env = env_with(&[("LOCALHOLD_EMBEDDING_MODEL", "test-model")]);
    let mut config = Config::default();
    config.apply_env_from_map(&env).unwrap();
    assert_eq!(config.embedding.openai_compatible().unwrap().model, "test-model");
}

#[test]
fn reranker_provider_policy_loads_from_toml_and_env() {
    let mut config: Config = toml::from_str("[search.reranker]\nenabled = true\nexecution_provider = \"cpu\"\nprecision = \"fp32\"\nrequired = false\n").unwrap();
    assert_eq!(config.search.reranker.execution_provider, RerankerExecutionProvider::Cpu);
    assert_eq!(config.search.reranker.precision, RerankerPrecision::Fp32);
    assert!(!config.search.reranker.required);

    config
        .apply_env_from_map(&env_with(&[
            ("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cuda"),
            ("LOCALHOLD_RERANKER_PRECISION", "fp16"),
            ("LOCALHOLD_RERANKER_REQUIRED", "true"),
        ]))
        .unwrap();
    assert_eq!(config.search.reranker.execution_provider, RerankerExecutionProvider::Cuda);
    assert_eq!(config.search.reranker.precision, RerankerPrecision::Fp16);
    assert!(config.search.reranker.required);
}

#[test]
fn reranker_provider_policy_rejects_unknown_value() {
    let error = toml::from_str::<Config>("[search.reranker]\nenabled = true\nexecution_provider = \"tpu\"\n").unwrap_err();
    assert!(error.to_string().contains("unknown variant `tpu`"));
}

#[test]
fn reranker_precision_rejects_unknown_value() {
    let error = toml::from_str::<Config>("[search.reranker]\nprecision = \"bf16\"\n").unwrap_err();
    assert!(error.to_string().contains("unknown variant `bf16`"));
}

#[test]
fn env_overrides_keep_rerank_top_m_and_deprecated_pool_size_separate() {
    let env = env_with(&[("LOCALHOLD_RERANK_TOP_M", "25"), ("LOCALHOLD_RERANKER_POOL_SIZE", "40")]);
    let mut config = Config::default();
    config.apply_env_from_map(&env).unwrap();
    config.validate(&std::collections::HashMap::new()).unwrap();

    assert_eq!(config.search.rerank_top_m, 25);
    assert_eq!(config.search.reranker.pool_size, 40);
}

#[test]
fn env_overrides_apply_all_fields() {
    let env = env_with(&[
        ("LOCALHOLD_DB_BACKEND", "postgres"),
        ("LOCALHOLD_DB_PATH", "/tmp/localhold-test.db"),
        ("LOCALHOLD_POSTGRES_URL", "postgresql://localhold:secret@localhost:5433/localhold_test"),
        ("LOCALHOLD_POSTGRES_MAX_CONNECTIONS", "12"),
        ("LOCALHOLD_POSTGRES_MIGRATION_LOCK_TIMEOUT_SECS", "23"),
        ("LOCALHOLD_POSTGRES_AUTO_MIGRATE", "false"),
        ("LOCALHOLD_EMBEDDING_BASE_URL", "http://example.local/v1"),
        ("LOCALHOLD_EMBEDDING_MODEL", "embed-model"),
        ("LOCALHOLD_EMBEDDING_API_KEY", "embed-key"),
        ("LOCALHOLD_EMBEDDING_AUTH_MODE", "api_key"),
        ("LOCALHOLD_EMBEDDING_SEND_DIMENSIONS", "true"),
        ("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled"),
        ("LOCALHOLD_EMBEDDING_ALLOW_INSECURE_HTTP", "true"),
        ("LOCALHOLD_LOG_LEVEL", "debug"),
        ("LOCALHOLD_PRINCIPAL", "configured-agent"),
        ("LOCALHOLD_ANONYMOUS_POLICY", "deny_all"),
        ("LOCALHOLD_TRANSPORT", "http"),
        ("LOCALHOLD_HTTP_HOST", "0.0.0.0"),
        ("LOCALHOLD_HTTP_PORT", "8090"),
        ("LOCALHOLD_HTTP_PATH", "/memory"),
        ("LOCALHOLD_HTTP_AUTH_TOKEN", "secret-token"),
        ("LOCALHOLD_HTTP_PRINCIPAL_MODE", "trusted_proxy"),
        ("LOCALHOLD_HTTP_PRINCIPAL", "proxy-fallback"),
        ("LOCALHOLD_HTTP_PRINCIPAL_HEADER", "X-Agent-Principal"),
        ("LOCALHOLD_HTTP_ALLOWED_HOSTS", "localhold.internal, 10.0.0.4:8080"),
        ("LOCALHOLD_HTTP_MAX_BODY_BYTES", "4096"),
        ("LOCALHOLD_HTTP_MAX_SESSIONS", "24"),
        ("LOCALHOLD_HTTP_SESSION_IDLE_TIMEOUT_SECS", "600"),
        ("LOCALHOLD_ADMIN_TOOLS_ENABLED", "true"),
    ]);

    let mut config = Config::default();
    config.apply_env_from_map(&env).unwrap();

    let embedding = config.embedding.openai_compatible().unwrap();
    assert_eq!(config.database.backend, DatabaseBackend::Postgres);
    assert_eq!(config.database.sqlite_path(), Path::new("/tmp/localhold-test.db"));
    assert_eq!(config.database.postgres.url, "postgresql://localhold:secret@localhost:5433/localhold_test");
    assert_eq!(config.database.postgres.max_connections, 12);
    assert_eq!(config.database.postgres.migration_lock_timeout_secs, 23);
    assert!(!config.database.postgres.auto_migrate);
    assert_eq!(embedding.base_url, "http://example.local/v1");
    assert_eq!(embedding.model, "embed-model");
    assert_eq!(embedding.api_key.as_deref(), Some("embed-key"));
    assert_eq!(embedding.auth_mode, OpenAiAuthMode::ApiKey);
    assert!(embedding.send_dimensions);
    assert_eq!(embedding.health_check, EmbeddingHealthCheck::Disabled);
    assert!(embedding.allow_insecure_http);
    assert_eq!(config.server.log_level, "debug");
    assert_eq!(config.server.principal.as_deref(), Some("configured-agent"));
    assert_eq!(config.server.anonymous_policy, AnonymousPolicy::DenyAll);
    assert_eq!(config.server.transport, Transport::Http);
    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.server.port, 8090);
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
fn malformed_typed_env_override_is_rejected_without_echoing_value() {
    for (key, secret) in [
        ("LOCALHOLD_POSTGRES_AUTO_MIGRATE", "not-a-boolean-secret"),
        ("LOCALHOLD_POSTGRES_MIGRATION_LOCK_TIMEOUT_SECS", "not-a-timeout-secret"),
        ("LOCALHOLD_EMBEDDING_AUTH_MODE", "not-an-auth-mode-secret"),
        ("LOCALHOLD_EMBEDDING_SEND_DIMENSIONS", "not-a-boolean-secret"),
        ("LOCALHOLD_EMBEDDING_DIMENSIONS", "not-a-dimension-secret"),
        ("LOCALHOLD_LOG_LEVEL", "not-a-filter[secret"),
        ("LOCALHOLD_HTTP_PRINCIPAL_HEADER", "invalid header secret"),
        ("LOCALHOLD_HTTP_ALLOWED_HOSTS", "invalid host secret"),
    ] {
        let env = env_with(&[(key, secret)]);
        let error = Config::load_from_sources(&[], &env).unwrap_err();
        let message = error.to_string();
        assert!(matches!(error, EngineError::Config(_)));
        assert!(message.contains(key), "unexpected error for {key}: {message}");
        assert!(message.contains("environment override is malformed"));
        assert!(!message.contains(secret));
    }
}

#[test]
fn env_overrides_apply_limits() {
    let env = env_with(&[
        ("LOCALHOLD_MAX_SEARCH_LIMIT", "42"),
        ("LOCALHOLD_MAX_CANDIDATE_POOL_SIZE", "500"),
        ("LOCALHOLD_MAX_LIST_LIMIT", "1000"),
        ("LOCALHOLD_MAX_CONTENT_LENGTH", "131072"),
        ("LOCALHOLD_MAX_TAGS_PER_MEMORY", "25"),
        ("LOCALHOLD_MAX_TAG_LENGTH", "128"),
        ("LOCALHOLD_MAX_BATCH_SIZE", "50"),
        ("LOCALHOLD_MAX_REEMBED_LIMIT", "75"),
        ("LOCALHOLD_EMBEDDING_TIMEOUT", "60"),
        ("LOCALHOLD_MAX_CONCURRENT_EMBEDDING_REQUESTS", "3"),
        ("LOCALHOLD_EMBEDDING_BATCH_SIZE", "12"),
        ("LOCALHOLD_EMBEDDING_MAX_RETRIES", "4"),
        ("LOCALHOLD_EMBEDDING_RETRY_INITIAL_BACKOFF_MS", "250"),
        ("LOCALHOLD_EMBEDDING_RETRY_MAX_BACKOFF_MS", "12000"),
        ("LOCALHOLD_SHUTDOWN_TIMEOUT", "15"),
        ("LOCALHOLD_MAX_TOP_TAGS_LIMIT", "50"),
        ("LOCALHOLD_MAX_HISTORY_LIMIT", "250"),
    ]);

    let mut config = Config::default();
    config.apply_env_from_map(&env).unwrap();

    assert_eq!(config.limits.max_search_limit, 42);
    assert_eq!(config.limits.max_candidate_pool_size, 500);
    assert_eq!(config.limits.max_list_limit, 1000);
    assert_eq!(config.limits.max_content_length, 131_072);
    assert_eq!(config.limits.max_tags_per_memory, 25);
    assert_eq!(config.limits.max_tag_length, 128);
    assert_eq!(config.limits.max_batch_size, 50);
    assert_eq!(config.limits.max_reembed_limit, 75);
    assert_eq!(config.limits.embedding_timeout_secs, 60);
    assert_eq!(config.limits.max_concurrent_embedding_requests, 3);
    assert_eq!(config.limits.embedding_batch_size, 12);
    assert_eq!(config.limits.embedding_max_retries, 4);
    assert_eq!(config.limits.embedding_retry_initial_backoff_ms, 250);
    assert_eq!(config.limits.embedding_retry_max_backoff_ms, 12_000);
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
    let toml_str = "[database]\nbackend = \"sqlite\"\n\n[database.sqlite]\npath = \"/tmp/custom-localhold.db\"\n";
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.database.backend, DatabaseBackend::Sqlite);
    assert_eq!(config.database.sqlite_path(), Path::new("/tmp/custom-localhold.db"));
}

#[test]
fn database_config_legacy_path_alias_from_toml() {
    let toml_str = "[database]\npath = \"/tmp/legacy-localhold.db\"\n";
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.database.backend, DatabaseBackend::Sqlite);
    assert_eq!(config.database.sqlite_path(), Path::new("/tmp/legacy-localhold.db"));
}

#[test]
fn database_config_postgres_from_toml() {
    let toml_str = "[database]\nbackend = \"postgres\"\n\n[database.postgres]\nurl = \"postgresql://localhold:secret@localhost:5433/localhold_test\"\nmax_connections = 9\nmigration_lock_timeout_secs = 23\nauto_migrate = false\n";
    let mut config: Config = toml::from_str(toml_str).unwrap();
    config.validate(&no_env()).unwrap();
    assert_eq!(config.database.backend, DatabaseBackend::Postgres);
    assert_eq!(config.database.postgres.url, "postgresql://localhold:secret@localhost:5433/localhold_test");
    assert_eq!(config.database.postgres.max_connections, 9);
    assert_eq!(config.database.postgres.migration_lock_timeout_secs, 23);
    assert!(!config.database.postgres.auto_migrate);
}

#[test]
fn database_config_legacy_postgres_toml_uses_default_migration_lock_timeout() {
    let config: Config = toml::from_str("[database.postgres]\nmax_connections = 9\nauto_migrate = false\n").unwrap();
    assert_eq!(config.database.postgres.migration_lock_timeout_secs, 5);
}

#[test]
fn embedding_config_openai_compatible_from_toml() {
    let toml_str = "[embedding]\nprovider = \"openai_compatible\"\ndimensions = 384\n\n[embedding.openai_compatible]\nbase_url = \"https://remote.example/v1\"\nmodel = \"custom-model\"\napi_key = \"secret\"\nauth_mode = \"api_key\"\nsend_dimensions = true\nhealth_check = \"disabled\"\n";
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.embedding.dimensions(), 384);
    let embedding = config.embedding.openai_compatible().unwrap();
    assert_eq!(embedding.base_url, "https://remote.example/v1");
    assert_eq!(embedding.model, "custom-model");
    assert_eq!(embedding.api_key.as_deref(), Some("secret"));
    assert_eq!(embedding.auth_mode, OpenAiAuthMode::ApiKey);
    assert!(embedding.send_dimensions);
    assert_eq!(embedding.health_check, EmbeddingHealthCheck::Disabled);
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
    config.database.path = Some(PathBuf::from("~/data/legacy-localhold.db"));
    config.database.sqlite.path = PathBuf::from("~/data/localhold.db");
    config.resolve_paths().unwrap();
    assert!(!config.database.path.as_ref().unwrap().to_str().unwrap().starts_with("~/"));
    assert!(!config.database.sqlite.path.to_str().unwrap().starts_with("~/"));
}

#[test]
fn validate_database_config_rejects_bad_active_postgres_url() {
    let mut config = DatabaseConfig {
        backend: DatabaseBackend::Postgres,
        postgres: PostgresDatabaseConfig {
            url: "http://localhost:5432/localhold".into(),
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
fn validate_database_config_enforces_postgres_migration_lock_timeout_bounds() {
    let config_with_timeout = |migration_lock_timeout_secs| DatabaseConfig {
        backend: DatabaseBackend::Postgres,
        postgres: PostgresDatabaseConfig {
            migration_lock_timeout_secs,
            ..PostgresDatabaseConfig::default()
        },
        ..DatabaseConfig::default()
    };

    validate_database_config(&mut config_with_timeout(MAX_POSTGRES_MIGRATION_LOCK_TIMEOUT_SECS)).unwrap();
    for migration_lock_timeout_secs in [0, MAX_POSTGRES_MIGRATION_LOCK_TIMEOUT_SECS + 1] {
        let mut config = config_with_timeout(migration_lock_timeout_secs);
        let err = validate_database_config(&mut config).unwrap_err();
        assert!(err.to_string().contains("database.postgres.migration_lock_timeout_secs"));
    }
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
                ..OpenAiCompatibleConfig::default()
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
fn user_config_candidates_load_canonical_file() {
    let root = unique_temp_dir("config-sources-canonical");
    let localhold_dir = root.join("localhold");
    fs::create_dir_all(&localhold_dir).unwrap();

    fs::write(
        localhold_dir.join("localhold.toml"),
        "[embedding]\nprovider = \"openai_compatible\"\n\n[embedding.openai_compatible]\nmodel = \"canonical\"\n",
    )
    .unwrap();
    let paths = user_config_candidates(Some(&root));
    let config = Config::load_from_sources(&paths, &no_env()).unwrap();
    assert_eq!(config.embedding.openai_compatible().unwrap().model, "canonical");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn user_config_candidates_never_include_current_directory_files() {
    let root = unique_temp_dir("config-sources-no-cwd");
    let paths = user_config_candidates(Some(&root));

    assert_eq!(paths, [root.join("localhold/localhold.toml")]);
    assert!(paths.iter().all(|path| path.is_absolute()));
    assert!(!paths.iter().any(|path| path == Path::new("localhold.toml")));
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
        ("LOCALHOLD_EMBEDDING_MODEL", "env-model"),
        ("LOCALHOLD_MAX_BATCH_SIZE", "25"),
        ("LOCALHOLD_PRINCIPAL", " "),
        ("LOCALHOLD_HTTP_AUTH_TOKEN", "  "),
        ("LOCALHOLD_HTTP_PRINCIPAL_HEADER", " X-LocalHold-Principal "),
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
    let env = env_with(&[("LOCALHOLD_HTTP_PRINCIPAL_HEADER", "bad header")]);
    let err = Config::load_from_sources(&[], &env).unwrap_err();
    assert!(err.to_string().contains("LOCALHOLD_HTTP_PRINCIPAL_HEADER"));
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
    let env = env_with(&[("LOCALHOLD_EMBEDDING_MODEL", "from-env")]);
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
    assert_zero_limit_rejected("max_concurrent_embedding_requests", |limits| limits.max_concurrent_embedding_requests = 0);
    assert_zero_limit_rejected("embedding_batch_size", |limits| limits.embedding_batch_size = 0);
    assert_zero_limit_rejected("embedding_retry_initial_backoff_ms", |limits| limits.embedding_retry_initial_backoff_ms = 0);
    assert_zero_limit_rejected("embedding_retry_max_backoff_ms", |limits| limits.embedding_retry_max_backoff_ms = 0);
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
fn validate_limits_config_accepts_zero_embedding_retries() {
    let limits = LimitsConfig {
        embedding_max_retries: 0,
        ..LimitsConfig::default()
    };
    validate_limits_config(&limits).unwrap();
}

#[test]
fn validate_limits_config_rejects_excessive_embedding_retries() {
    let limits = LimitsConfig {
        embedding_max_retries: 11,
        ..LimitsConfig::default()
    };
    let error = validate_limits_config(&limits).unwrap_err();
    assert!(error.to_string().contains("embedding_max_retries"));
}

#[test]
fn validate_limits_config_rejects_inverted_embedding_backoff() {
    let limits = LimitsConfig {
        embedding_retry_initial_backoff_ms: 2_000,
        embedding_retry_max_backoff_ms: 1_000,
        ..LimitsConfig::default()
    };
    let error = validate_limits_config(&limits).unwrap_err();
    assert!(error.to_string().contains("embedding_retry_max_backoff_ms"));
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
fn validate_openai_compatible_config_requires_https_for_remote_host() {
    let config = OpenAiCompatibleConfig {
        base_url: "http://embeddings.example/v1".into(),
        ..OpenAiCompatibleConfig::default()
    };
    let err = validate_openai_compatible_config(&config).unwrap_err();
    assert!(err.to_string().contains("must use https"));
}

#[test]
fn validate_openai_compatible_config_allows_explicit_trusted_http() {
    let config = OpenAiCompatibleConfig {
        base_url: "http://embeddings.internal/v1".into(),
        allow_insecure_http: true,
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
fn validate_search_config_required_reranker_must_be_enabled() {
    let config = SearchConfig {
        reranker: RerankerConfig {
            required: true,
            ..RerankerConfig::default()
        },
        ..SearchConfig::default()
    };
    let err = validate_search_config(&config).unwrap_err();
    assert!(err.to_string().contains("reranker.required = true requires reranker.enabled = true"));
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
fn validate_search_config_fp16_requires_explicit_cuda() {
    for execution_provider in [RerankerExecutionProvider::Auto, RerankerExecutionProvider::Cpu] {
        let config = SearchConfig {
            reranker: RerankerConfig {
                enabled: true,
                execution_provider,
                precision: RerankerPrecision::Fp16,
                ..RerankerConfig::default()
            },
            ..SearchConfig::default()
        };
        let err = validate_search_config(&config).unwrap_err();
        assert!(err.to_string().contains("requires reranker.execution_provider = \"cuda\""));
    }

    let config = SearchConfig {
        reranker: RerankerConfig {
            enabled: true,
            execution_provider: RerankerExecutionProvider::Cuda,
            precision: RerankerPrecision::Fp16,
            ..RerankerConfig::default()
        },
        ..SearchConfig::default()
    };
    validate_search_config(&config).unwrap();
}

#[test]
fn validate_search_config_managed_fp16_rejects_custom_download_pins() {
    let config = SearchConfig {
        reranker: RerankerConfig {
            enabled: true,
            execution_provider: RerankerExecutionProvider::Cuda,
            precision: RerankerPrecision::Fp16,
            model_sha256: "custom".into(),
            ..RerankerConfig::default()
        },
        ..SearchConfig::default()
    };
    let err = validate_search_config(&config).unwrap_err();
    assert!(err.to_string().contains("managed fp16 reranker artifact"));
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
