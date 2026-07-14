//! Interactive terminal UI (`hold ui`) for browsing and searching the hold.
//!
//! The UI opens the configured store directly (SQLite in WAL mode supports
//! concurrent readers alongside a running server; `PostgreSQL` is shared by
//! nature), so it works whether or not a `LocalHold` process is serving MCP.
//! It is read-only: browsing records no activity and writes nothing.

mod app;
mod theme;
mod view;

use std::{fmt, sync::Arc};

use ratatui::crossterm::event::{self, Event};
use tokio::sync::mpsc;

use crate::{
    clock::{Clock, SystemClock},
    config::{AnonymousPolicy, Config, DatabaseBackend},
    embedding::factory::{active_embedding_profile, create_embedding_provider_with_clock},
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
    /// Principal used for read visibility; defaults to `server.principal`.
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
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    match config.database.backend {
        DatabaseBackend::Sqlite => {
            let db_path = config.database.sqlite_path().to_path_buf();
            let store = SqliteStore::open_read_only_with_clock(&db_path, config.embedding.dimensions(), Arc::clone(&clock))?;
            if let Some(profile) = active_embedding_profile(&config.embedding) {
                store.check_embedding_profile(&profile).await?;
            }
            run_with_store(store, config, clock, options).await
        }
        DatabaseBackend::Postgres => {
            let store = PostgresStore::open_read_only_with_clock(&config.database.postgres, config.embedding.dimensions(), Arc::clone(&clock)).await?;
            if let Some(profile) = active_embedding_profile(&config.embedding) {
                store.check_embedding_profile(&profile).await?;
            }
            run_with_store(store, config, clock, options).await
        }
    }
}

async fn run_with_store<S>(store: S, config: Config, clock: Arc<dyn Clock>, options: UiOptions) -> Result<i32, UiError>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let principal = resolve_principal(&config, options)?;

    #[cfg(feature = "reranker")]
    let reranker_config = config.search.reranker.clone();

    #[cfg(not(feature = "reranker"))]
    if config.search.reranker.enabled {
        let requested = config.search.reranker.execution_provider;
        if config.search.reranker.required {
            return Err(localhold_reranker_unavailable(requested).into());
        }
        tracing::warn!(%requested, "reranker requested but this binary was compiled without reranker support; TUI search will not rerank");
    }

    let embedding = create_embedding_provider_with_clock(&config.embedding, &config.limits, None, Arc::clone(&clock)).await;
    let engine = LocalHoldEngine::new_with_clock(store, embedding, config.limits, config.search, Arc::clone(&clock));

    #[cfg(feature = "reranker")]
    let engine = if reranker_config.enabled {
        match crate::reranker::runtime::initialize_with_retry_and_clock(&reranker_config, clock).await {
            Ok(reranker) => engine.with_reranker(reranker.into_provider()),
            Err(error) if reranker_config.required => return Err(error.into()),
            Err(error) => {
                tracing::warn!(%error, "optional TUI reranker initialization failed; search will continue without reranking");
                engine
            }
        }
    } else {
        engine
    };

    let terminal = ratatui::try_init()?;
    let result = {
        let _restore = TerminalRestoreGuard;
        event_loop(terminal, engine.clone(), principal).await
    };
    engine.shutdown().await;
    result
}

fn resolve_principal(config: &Config, options: UiOptions) -> Result<Option<String>, EngineError> {
    let principal = options.principal.or_else(|| config.server.principal.clone());
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
async fn event_loop<S>(mut terminal: ratatui::DefaultTerminal, engine: LocalHoldEngine<S>, principal: Option<String>) -> Result<i32, UiError>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let (data_tx, mut data_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    spawn_input_reader(event_tx);
    let mut app = app::App::new(engine, theme::Theme::detect(), principal, data_tx);
    app.bootstrap().await;
    while !app.quit {
        let _completed_frame = terminal.draw(|frame| view::draw(frame, &app))?;
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
        assert_eq!(resolve_principal(&config, UiOptions::new(Some("operator".into()))).unwrap().as_deref(), Some("operator"));
    }
}
