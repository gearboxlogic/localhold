//! `LocalHold` binary — starts the MCP server over stdio or HTTP transport.

use std::{
    ffi::OsString,
    future::{Future, IntoFuture},
    io::Write as _,
    sync::Arc,
};

use localhold::{
    config::{Config, DatabaseBackend, HttpPrincipalMode, ServerConfig, Transport},
    embedding::factory::{active_embedding_profile, create_embedding_provider},
    engine::{LocalHoldEngine, ReembedOutcome, ReembedRequest},
    error::EngineError,
    http_transport::build_router,
    server::{HttpPrincipalSource, LocalHoldServer},
    store::{
        MemoryStore, PostgresStore, SqliteStore,
        migration::{MigrationError, SqliteToPostgresOptions, migrate_sqlite_to_postgres},
    },
};
use rmcp::ServiceExt as _;
use tokio::sync::Notify;
use tracing::{info, warn};

type AppResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[tokio::main]
async fn main() -> AppResult {
    if let Some(result) = try_run_info_cli() {
        return result;
    }
    if let Some(result) = try_run_doctor_cli().await {
        match result {
            Ok(0_i32) => return Ok(()),
            Ok(exit_code) => std::process::exit(exit_code),
            Err(error) => {
                write_stderr_line(error);
                std::process::exit(1);
            }
        }
    }
    if let Some(result) = try_run_migration_cli().await {
        if let Err(error) = result {
            write_migration_cli_error(&*error);
            std::process::exit(1);
        }
        return Ok(());
    }

    if let Some(result) = try_run_embeddings_cli().await {
        if let Err(error) = result {
            write_stderr_line(error);
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(argument) = std::env::args_os().nth(1) {
        write_stderr_line(root_usage());
        return Err(EngineError::config(format!("unknown argument: {}", argument.to_string_lossy())).into());
    }

    // Load config
    let config = Config::load()?;

    // Init tracing to stderr (stdout is reserved for MCP stdio transport)
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| parse_log_level(&config.server.log_level));
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(env_filter).init();

    info!("localhold starting up");
    let embedding_profile = active_embedding_profile(&config.embedding);

    match config.database.backend {
        DatabaseBackend::Sqlite => {
            let db_path = config.database.sqlite_path().to_path_buf();
            if let Some(parent) = db_path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            let store = SqliteStore::open(&db_path, config.embedding.dimensions())?;
            if let Some(profile) = &embedding_profile {
                store.verify_embedding_profile(profile).await?;
            }
            info!("sqlite database opened at {}", db_path.display());
            run_with_store(store, config).await
        }
        DatabaseBackend::Postgres => {
            let store = PostgresStore::open(&config.database.postgres, config.embedding.dimensions()).await?;
            if let Some(profile) = &embedding_profile {
                store.verify_embedding_profile(profile).await?;
            }
            info!("postgres database opened");
            run_with_store(store, config).await
        }
        other => return Err(EngineError::config(format!("unsupported database backend: {other}")).into()),
    }
}

fn try_run_info_cli() -> Option<AppResult> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [arg] if is_help_arg(arg) => Some(write_stdout(root_usage()).and_then(|()| write_stdout("\n"))),
        [arg] if arg == "-V" || arg == "--version" => Some(write_stdout(concat!("hold ", env!("CARGO_PKG_VERSION"), "\n"))),
        _ => None,
    }
}

const fn root_usage() -> &'static str {
    "Usage: hold [COMMAND]\n\nRuns the LocalHold MCP server when no command is supplied.\n\nCommands:\n  doctor                     Diagnose installation and runtime readiness\n  embeddings reindex --yes   Clear and rebuild the configured vector space\n  migrate sqlite-to-postgres Migrate storage backends\n\nOptions:\n  -h, --help                 Print help\n  -V, --version              Print version"
}

async fn try_run_doctor_cli() -> Option<Result<i32, Box<dyn std::error::Error + Send + Sync>>> {
    const USAGE: &str = "Usage: hold doctor [--json] [--allow-downloads]\n\nRuns side-effect-conscious readiness checks. By default, doctor does not create databases, migrate schemas, or download reranker artifacts.\n\nOptions:\n  --json             Emit the stable JSON report schema\n  --allow-downloads  Permit first-use reranker downloads for inference probing\n  -h, --help         Print help\n\nExit codes:\n  0  healthy\n  1  failed\n  2  degraded";

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|argument| argument != "doctor") {
        return None;
    }
    Some(
        async {
            if args[1..].iter().any(is_help_arg) {
                write_stdout(USAGE)?;
                write_stdout("\n")?;
                return Ok(0);
            }
            let mut json = false;
            let mut allow_downloads = false;
            for argument in &args[1..] {
                if argument == "--json" {
                    json = true;
                } else if argument == "--allow-downloads" {
                    allow_downloads = true;
                } else {
                    return Err(EngineError::config(format!("unknown doctor argument: {}\n\n{USAGE}", argument.to_string_lossy())).into());
                }
            }
            let mut options = localhold::doctor::DoctorOptions::default();
            options.allow_downloads = allow_downloads;
            let report = localhold::doctor::run(options).await;
            if json {
                write_stdout(&report.to_json()?)?;
            } else {
                write_stdout(&report.render_text())?;
            }
            Ok(report.exit_code)
        }
        .await,
    )
}

async fn try_run_embeddings_cli() -> Option<AppResult> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|arg| arg != "embeddings") {
        return None;
    }
    Some(run_embeddings_cli(&args[1..]).await)
}

async fn run_embeddings_cli(args: &[OsString]) -> AppResult {
    const USAGE: &str = "Usage: hold embeddings reindex --yes\n\nClears stored vectors while preserving memories, then records the embedding provider, endpoint, model, and dimensions from localhold.toml.";
    if args.iter().any(is_help_arg) {
        write_stdout(USAGE)?;
        write_stdout("\n")?;
        return Ok(());
    }
    if args.first().is_none_or(|arg| arg != "reindex") {
        write_stderr_line(USAGE);
        return Err(EngineError::config("missing or unknown embeddings command").into());
    }
    if !args[1..].iter().any(|arg| arg == "--yes") {
        return Err(EngineError::config("reindex is destructive to stored vectors; rerun with `--yes` to confirm").into());
    }
    if args.len() != 2 {
        return Err(EngineError::config("unexpected embeddings reindex argument").into());
    }

    let config = Config::load()?;
    let profile = active_embedding_profile(&config.embedding).ok_or_else(|| EngineError::config("embeddings reindex requires an active OpenAI-compatible embedding provider"))?;
    match config.database.backend {
        DatabaseBackend::Sqlite => {
            let path = config.database.sqlite_path();
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            SqliteStore::reindex_embeddings(path, &profile).await?;
        }
        DatabaseBackend::Postgres => PostgresStore::reindex_embeddings(&config.database.postgres, &profile).await?,
        other => return Err(EngineError::config(format!("unsupported database backend: {other}")).into()),
    }
    write_stdout("Embedding vectors cleared. Start LocalHold to rebuild them with the configured provider.\n")?;
    Ok(())
}

async fn try_run_migration_cli() -> Option<AppResult> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|arg| arg != "migrate") {
        return None;
    }
    Some(run_migration_cli(&args[1..]).await)
}

async fn run_migration_cli(args: &[OsString]) -> AppResult {
    if args.first().is_some_and(is_help_arg) {
        write_stdout(localhold::store::migration::usage())?;
        write_stdout("\n")?;
        return Ok(());
    }
    let Some(command) = args.first() else {
        write_stderr_line(localhold::store::migration::usage());
        return Err(EngineError::config("missing migration command").into());
    };
    if command != "sqlite-to-postgres" {
        write_stderr_line(localhold::store::migration::usage());
        return Err(EngineError::config(format!("unknown migration command: {}", command.to_string_lossy())).into());
    }
    if args[1..].iter().any(is_help_arg) {
        write_stdout(localhold::store::migration::usage())?;
        write_stdout("\n")?;
        return Ok(());
    }

    let options = SqliteToPostgresOptions::parse_args(&args[1..]).map_err(migration_error_to_box)?;
    let summary = migrate_sqlite_to_postgres(&options).await.map_err(migration_error_to_box)?;
    write_stdout(&summary.render())?;
    Ok(())
}

fn is_help_arg(arg: &OsString) -> bool {
    arg == "-h" || arg == "--help"
}

fn migration_error_to_box(error: MigrationError) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(error)
}

fn write_migration_cli_error(error: &(dyn std::error::Error + Send + Sync + 'static)) {
    match error.downcast_ref::<MigrationError>() {
        Some(MigrationError::Usage(message)) => write_stderr_line(message),
        Some(error) => write_stderr_line(format_args!("migration failed: {error}")),
        None => write_stderr_line(error),
    }
}

fn write_stdout(message: &str) -> AppResult {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(message.as_bytes())?;
    Ok(())
}

fn write_stderr_line(message: impl std::fmt::Display) {
    let mut stderr = std::io::stderr().lock();
    let _write_failed = writeln!(stderr, "{message}").is_err();
}

async fn run_with_store<S>(store: S, config: Config) -> AppResult
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    let embedding_recovery_enabled = active_embedding_profile(&config.embedding).is_some();

    // Create embedding provider with recovery notification
    let recovery_notify = Arc::new(Notify::new());
    let embedding = create_embedding_provider(&config.embedding, &config.limits, Some(Arc::clone(&recovery_notify))).await;

    // Clone reranker config before search config is consumed by LocalHoldEngine::new
    #[cfg(feature = "reranker")]
    let reranker_config = config.search.reranker.clone();

    // Enforce reranker requirements before the server starts when support was
    // not compiled into this binary.
    #[cfg(not(feature = "reranker"))]
    if config.search.reranker.enabled {
        let requested = config.search.reranker.execution_provider;
        let required = config.search.reranker.required;
        let inactive = "none";
        if required {
            return Err(localhold::reranker::RerankerError::ProviderUnavailable(format!(
                "{requested} was requested with reranker.required = true, but this binary was compiled without the `reranker` feature"
            ))
            .into());
        }
        warn!(
            compiled = "none",
            %requested,
            required,
            selected = %inactive,
            active = %inactive,
            "reranker.enabled = true but compiled without `reranker` feature -- reranking disabled"
        );
    }

    let server_principal = config.server.principal.clone();
    let anonymous_policy = config.server.anonymous_policy;
    let http_auth_token = config.server.http_auth_token.clone();
    let admin_tools_enabled = config.server.admin_tools_enabled;
    let http_principal_source = match config.server.http_principal_mode {
        HttpPrincipalMode::Fixed => HttpPrincipalSource::fixed(config.server.http_principal.clone()),
        HttpPrincipalMode::TrustedProxy => HttpPrincipalSource::trusted_proxy_header(config.server.http_principal_header.clone()),
        _ => return Err("unsupported HTTP principal mode".into()),
    };

    let engine = LocalHoldEngine::new(store, embedding, config.limits, config.search);

    // Optionally attach a cross-encoder reranker (reranker feature)
    #[cfg(feature = "reranker")]
    let engine = if reranker_config.enabled {
        match localhold::reranker::runtime::initialize_with_retry(&reranker_config).await {
            Ok(reranker) => engine.with_reranker(reranker.into_provider()),
            Err(error) if reranker_config.required => return Err(error.into()),
            Err(e) => {
                let inactive = "none";
                let compiled = localhold::reranker::policy::compiled_execution_providers()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                warn!(
                    %compiled,
                    requested = %reranker_config.execution_provider,
                    required = reranker_config.required,
                    selected = %inactive,
                    active = %inactive,
                    "reranker initialization failed after retries, continuing without: {e}"
                );
                engine
            }
        }
    } else {
        engine
    };

    // The noop provider is intentionally disabled and can never recover.
    if embedding_recovery_enabled {
        spawn_recovery_reembed(engine.clone(), recovery_notify);
    }

    let server = LocalHoldServer::from_engine_with_auth_and_http(engine, server_principal, anonymous_policy, http_auth_token, http_principal_source);
    let server = if admin_tools_enabled { server.with_admin_tools() } else { server };

    match config.server.transport {
        Transport::Stdio => Box::pin(serve_stdio(server)).await,
        Transport::Http => serve_http(server, &config.server).await,
        other => Err(format!("unsupported transport: {other}").into()),
    }
}

/// Spawn a background task that re-embeds unembedded memories whenever the
/// embedding provider recovers from an outage.
fn spawn_recovery_reembed<S>(engine: LocalHoldEngine<S>, notify: Arc<Notify>)
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    #[expect(unused_results, reason = "JoinHandle intentionally dropped — recovery task runs for server lifetime")]
    tokio::spawn(async move {
        let startup_total = drain_unembedded(&engine).await;
        if startup_total > 0 {
            info!("startup auto-reembed complete: {startup_total} memories queued for embedding");
        }
        loop {
            notify.notified().await;
            info!("embedding provider recovered, auto-reembedding unembedded memories");
            let total = drain_unembedded(&engine).await;
            if total > 0 {
                info!("auto-reembed complete: {total} memories queued for embedding");
            }
        }
    });
}

/// Re-embed all unembedded memories in batches, returning the total queued.
async fn drain_unembedded<S>(engine: &LocalHoldEngine<S>) -> usize
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    let batch_size = engine.limits().max_reembed_limit;
    let mut total = 0_usize;
    loop {
        match engine.reembed(ReembedRequest::Bulk { limit: batch_size }).await {
            Ok(ReembedOutcome::Queued(0)) => return total,
            Ok(ReembedOutcome::Queued(n)) => {
                total = total.saturating_add(n);
                info!("auto-reembed: queued {n} (total {total}), checking for more");
            }
            Ok(outcome) => {
                info!("auto-reembed: {outcome:?}");
                return total;
            }
            Err(e) => {
                warn!("auto-reembed batch failed: {e}");
                return total;
            }
        }
    }
}

fn parse_log_level(level: &str) -> tracing_subscriber::EnvFilter {
    level.parse().unwrap_or_else(|e| {
        #[expect(unused_must_use, reason = "best-effort stderr warning before tracing is ready")]
        {
            writeln!(std::io::stderr(), "warning: invalid log_level '{level}', falling back to default: {e}");
        }
        tracing_subscriber::EnvFilter::default()
    })
}

async fn run_with_shutdown<T, E, Run, ShutdownFn, ShutdownFut>(run: Run, shutdown: ShutdownFn) -> Result<T, E>
where
    Run: IntoFuture<Output = Result<T, E>>,
    ShutdownFn: FnOnce() -> ShutdownFut,
    ShutdownFut: Future<Output = ()>,
{
    let result = run.into_future().await;
    shutdown().await;
    result
}

async fn serve_stdio<S>(server: LocalHoldServer<S>) -> AppResult
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    let server_for_shutdown = server.clone();
    let serve_result: AppResult = Box::pin(run_with_shutdown(
        async {
            let service = server.clone().serve(rmcp::transport::io::stdio()).await?;
            info!("localhold MCP server running on stdio");
            #[expect(unused_results, reason = "waiting() returns () on completion — nothing to use")]
            service.waiting().await?;
            Ok(())
        },
        move || async move {
            server_for_shutdown.shutdown().await;
        },
    ))
    .await;
    info!("localhold shutting down");
    serve_result
}

async fn serve_http<S>(server: LocalHoldServer<S>, config: &ServerConfig) -> AppResult
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    use tokio_util::sync::CancellationToken;

    let ct = CancellationToken::new();
    if config.http_auth_token.is_none() {
        warn!(anonymous_policy = %config.anonymous_policy, "HTTP MCP endpoint has no bearer authentication; requests will be anonymous");
    }
    let server_for_shutdown = server.clone();
    let path = config.path.clone();
    let router = build_router(server, config, &ct)?;

    let bind_addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    let local_addr = listener.local_addr()?;
    info!("localhold MCP server listening on http://{local_addr}{path}");

    let shutdown_ct = ct.clone();
    #[expect(unused_results, reason = "JoinHandle intentionally dropped — shutdown task runs independently")]
    tokio::spawn(async move {
        #[expect(clippy::let_underscore_must_use, reason = "ctrl_c error is non-actionable; we just cancel on signal")]
        #[expect(let_underscore_drop, reason = "Result dropped immediately is fine — no resources held")]
        let _ = tokio::signal::ctrl_c().await;
        info!("received ctrl-c, initiating graceful shutdown");
        shutdown_ct.cancel();
    });

    let serve_result = run_with_shutdown(
        axum::serve(listener, router).with_graceful_shutdown(async move { ct.cancelled().await }),
        move || async move {
            server_for_shutdown.shutdown().await;
        },
    )
    .await;
    info!("localhold shutting down");
    Ok(serve_result?)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use localhold::{
        config::{LimitsConfig, SearchConfig},
        embedding::{BoxFuture, EmbeddingProvider},
        engine::LocalHoldEngine,
        error::EmbeddingError,
        store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
        types::{AccessPolicy, Memory, Provenance},
    };

    use super::{drain_unembedded, run_with_shutdown};

    struct FixedEmbedding;

    impl EmbeddingProvider for FixedEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Ok(vec![1.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS]) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn run_with_shutdown_executes_cleanup_on_success() {
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let shutdown_called_ref = Arc::clone(&shutdown_called);

        let result = run_with_shutdown(async { Ok::<_, &'static str>(42_i32) }, move || async move {
            shutdown_called_ref.store(true, Ordering::SeqCst);
        })
        .await;

        assert_eq!(result.unwrap(), 42_i32);
        assert!(shutdown_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn run_with_shutdown_executes_cleanup_on_error() {
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let shutdown_called_ref = Arc::clone(&shutdown_called);

        let result = run_with_shutdown(async { Err::<(), _>("boom") }, move || async move {
            shutdown_called_ref.store(true, Ordering::SeqCst);
        })
        .await;

        assert_eq!(result.unwrap_err(), "boom");
        assert!(shutdown_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn drain_unembedded_queues_startup_backlog() {
        let store = SqliteStore::in_memory().unwrap();
        let memory = Memory::new_for_test("startup backlog".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        let id = store.store(&memory, None).await.unwrap();
        let engine = LocalHoldEngine::new(store.clone(), Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());

        let queued = drain_unembedded(&engine).await;
        assert_eq!(queued, 1_usize);

        engine.shutdown_for_test(std::time::Duration::from_secs(1)).await;
        let after = store.get(&id, None).await.unwrap().unwrap();
        assert!(after.has_embedding);
    }
}
