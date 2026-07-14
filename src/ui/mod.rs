//! Interactive terminal UI (`hold ui`) for browsing and managing the hold.
//!
//! The UI opens the configured store directly. SQLite WAL mode and `PostgreSQL`
//! both support concurrent access alongside a running server. Browsing remains
//! side-effect-free; explicit edit and delete commands use normal audited
//! authorization paths.

mod app;
mod editor;
mod theme;
mod view;

use std::{fmt, sync::Arc};

use ratatui::crossterm::event::{self, Event};
use tokio::sync::mpsc;

use crate::{
    clock::{Clock, SystemClock},
    config::{AnonymousPolicy, Config, DatabaseBackend},
    embedding::factory::{active_embedding_profile, create_deferred_embedding_provider_with_clock},
    engine::LocalHoldEngine,
    error::EngineError,
    store::{MemoryStore, PostgresStore, SqliteStore},
};

/// Boxed error type matching the binary's `AppResult` convention.
type UiError = Box<dyn std::error::Error + Send + Sync>;

/// Options for [`run`], parsed by the binary from `hold ui` arguments.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct UiOptions {
    /// Principal used for visibility and writes; defaults to `server.principal`.
    pub principal: Option<String>,
}

impl UiOptions {
    /// Build options with an optional principal override.
    #[must_use]
    pub const fn new(principal: Option<String>) -> Self {
        Self { principal }
    }
}

/// Load config, connect the engine to the configured backend, and run the UI.
///
/// # Errors
///
/// Returns an error when config loading, store opening, or terminal setup fails.
pub async fn run(options: UiOptions) -> Result<i32, UiError> {
    let config = Config::load()?;
    let principal = resolve_principal(&config, options)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    match config.database.backend {
        DatabaseBackend::Sqlite => {
            let db_path = config.database.sqlite_path().to_path_buf();
            let store = SqliteStore::open_read_only_with_clock(&db_path, config.embedding.dimensions(), Arc::clone(&clock))?;
            if let Some(profile) = active_embedding_profile(&config.embedding) {
                store.check_embedding_profile(&profile).await?;
            }
            let writer_config = config.clone();
            let writer_clock = Arc::clone(&clock);
            let mutation_factory: app::MutationEngineFactory<SqliteStore> = Arc::new(move || {
                let config = writer_config.clone();
                let clock = Arc::clone(&writer_clock);
                Box::pin(open_sqlite_mutation_engine(config, clock))
            });
            run_with_store(store, config, clock, principal, mutation_factory).await
        }
        DatabaseBackend::Postgres => {
            let store = PostgresStore::open_read_only_with_clock(&config.database.postgres, config.embedding.dimensions(), Arc::clone(&clock)).await?;
            if let Some(profile) = active_embedding_profile(&config.embedding) {
                store.check_embedding_profile(&profile).await?;
            }
            let writer_config = config.clone();
            let writer_clock = Arc::clone(&clock);
            let mutation_factory: app::MutationEngineFactory<PostgresStore> = Arc::new(move || {
                let config = writer_config.clone();
                let clock = Arc::clone(&writer_clock);
                Box::pin(open_postgres_mutation_engine(config, clock))
            });
            run_with_store(store, config, clock, principal, mutation_factory).await
        }
    }
}

async fn open_sqlite_mutation_engine(config: Config, clock: Arc<dyn Clock>) -> Result<LocalHoldEngine<SqliteStore>, String> {
    let store = SqliteStore::open_with_clock(config.database.sqlite_path(), config.embedding.dimensions(), Arc::clone(&clock))
        .map_err(|error| format!("write store unavailable: {error}"))?;
    if let Some(profile) = active_embedding_profile(&config.embedding) {
        store
            .verify_embedding_profile(&profile)
            .await
            .map_err(|error| format!("write store unavailable: {error}"))?;
    }
    let embedding = create_deferred_embedding_provider_with_clock(&config.embedding, &config.limits, Arc::clone(&clock));
    Ok(LocalHoldEngine::new_with_clock(store, embedding, config.limits, config.search, clock))
}

async fn open_postgres_mutation_engine(config: Config, clock: Arc<dyn Clock>) -> Result<LocalHoldEngine<PostgresStore>, String> {
    let store = PostgresStore::open_with_clock(&config.database.postgres, config.embedding.dimensions(), Arc::clone(&clock))
        .await
        .map_err(|error| format!("write store unavailable: {error}"))?;
    if let Some(profile) = active_embedding_profile(&config.embedding) {
        store
            .verify_embedding_profile(&profile)
            .await
            .map_err(|error| format!("write store unavailable: {error}"))?;
    }
    let embedding = create_deferred_embedding_provider_with_clock(&config.embedding, &config.limits, Arc::clone(&clock));
    Ok(LocalHoldEngine::new_with_clock(store, embedding, config.limits, config.search, clock))
}

async fn run_with_store<S>(store: S, config: Config, clock: Arc<dyn Clock>, principal: Option<String>, mutation_factory: app::MutationEngineFactory<S>) -> Result<i32, UiError>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut startup_notice = None;

    #[cfg(feature = "reranker")]
    let reranker_config = config.search.reranker.clone();

    #[cfg(not(feature = "reranker"))]
    if config.search.reranker.enabled {
        let requested = config.search.reranker.execution_provider;
        if config.search.reranker.required {
            return Err(localhold_reranker_unavailable(requested).into());
        }
        startup_notice = Some(format!("reranker off: {requested} support is not compiled into this binary"));
    }

    let embedding = create_deferred_embedding_provider_with_clock(&config.embedding, &config.limits, Arc::clone(&clock));
    let engine = LocalHoldEngine::new_with_clock(store, embedding, config.limits, config.search, Arc::clone(&clock));

    #[cfg(feature = "reranker")]
    let engine = if reranker_config.enabled {
        match crate::reranker::runtime::initialize_cached_with_clock(&reranker_config, clock).await {
            Ok(reranker) => engine.with_reranker(reranker.into_provider()),
            Err(crate::reranker::RerankerError::Unavailable) if reranker_config.required => {
                return Err(EngineError::config("required TUI reranker artifacts are not cached; run `hold models fetch --yes` before starting `hold ui`").into());
            }
            Err(crate::reranker::RerankerError::Unavailable) => {
                startup_notice = Some("reranker off: artifacts are not cached; run `hold models fetch --yes` to enable it".into());
                engine
            }
            Err(error) if reranker_config.required => return Err(error.into()),
            Err(error) => {
                startup_notice = Some(format!("reranker off: {error}"));
                engine
            }
        }
    } else {
        engine
    };

    let terminal = ratatui::try_init()?;
    let restore = TerminalRestoreGuard;
    let (data_tx, data_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    spawn_input_reader(event_tx);
    let mut app = app::App::new_with_mutation_factory(engine.clone(), theme::Theme::detect(), principal, data_tx, mutation_factory);
    app.notice = startup_notice;
    app.bootstrap().await;
    let result = event_loop(terminal, &mut app, data_rx, event_rx).await;
    drop(restore);
    app.shutdown_mutation_engine().await;
    engine.shutdown().await;
    result
}

fn resolve_principal(config: &Config, options: UiOptions) -> Result<Option<String>, EngineError> {
    let principal = options.principal.or_else(|| config.server.principal.clone()).and_then(|principal| {
        let principal = principal.trim();
        (!principal.is_empty()).then(|| principal.to_owned())
    });
    if principal.is_none() && config.server.anonymous_policy == AnonymousPolicy::DenyAll {
        return Err(EngineError::config(
            "hold ui requires --principal or server.principal when server.anonymous_policy = deny_all",
        ));
    }
    Ok(principal)
}

#[cfg(not(feature = "reranker"))]
fn localhold_reranker_unavailable(requested: crate::config::RerankerExecutionProvider) -> crate::reranker::RerankerError {
    crate::reranker::RerankerError::ProviderUnavailable(format!(
        "{requested} was requested with reranker.required = true, but this binary was compiled without the `reranker` feature"
    ))
}

struct TerminalRestoreGuard;

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

#[expect(clippy::integer_division_remainder_used, reason = "false positive from tokio::select! macro expansion")]
async fn event_loop<S>(
    mut terminal: ratatui::DefaultTerminal,
    app: &mut app::App<S>,
    mut data_rx: mpsc::UnboundedReceiver<app::DataMsg>,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
) -> Result<i32, UiError>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    while !app.quit {
        let _completed_frame = terminal.draw(|frame| view::draw(frame, app))?;
        tokio::select! {
            maybe_event = event_rx.recv() => match maybe_event {
                Some(event) => app.on_event(event).await,
                None => break,
            },
            maybe_msg = data_rx.recv() => match maybe_msg {
                Some(msg) => app.on_data(msg),
                None => break,
            },
        }
    }
    Ok(0_i32)
}

/// Read terminal events on a dedicated thread; the blocking read cannot live
/// on the async runtime.
fn spawn_input_reader(tx: mpsc::UnboundedSender<Event>) {
    #[expect(unused_results, reason = "JoinHandle intentionally dropped — the reader lives for the process lifetime")]
    std::thread::spawn(move || {
        while let Ok(terminal_event) = event::read() {
            if tx.send(terminal_event).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_all_requires_a_ui_principal() {
        let mut config = Config::default();
        config.server.principal = None;
        config.server.anonymous_policy = AnonymousPolicy::DenyAll;

        let error = resolve_principal(&config, UiOptions::default()).unwrap_err();
        assert!(error.to_string().contains("requires --principal"));
        let error = resolve_principal(&config, UiOptions::new(Some(" \t ".into()))).unwrap_err();
        assert!(error.to_string().contains("requires --principal"));
        assert_eq!(
            resolve_principal(&config, UiOptions::new(Some("  operator  ".into()))).unwrap().as_deref(),
            Some("operator")
        );
    }
}
