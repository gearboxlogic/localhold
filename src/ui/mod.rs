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
    config::{Config, DatabaseBackend},
    embedding::factory::create_embedding_provider_with_clock,
    engine::LocalHoldEngine,
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
            let store = SqliteStore::open_with_clock(&db_path, config.embedding.dimensions(), Arc::clone(&clock))?;
            run_with_store(store, config, clock, options).await
        }
        DatabaseBackend::Postgres => {
            let store = PostgresStore::open_with_clock(&config.database.postgres, config.embedding.dimensions(), Arc::clone(&clock)).await?;
            run_with_store(store, config, clock, options).await
        }
    }
}

async fn run_with_store<S>(store: S, config: Config, clock: Arc<dyn Clock>, options: UiOptions) -> Result<i32, UiError>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let principal = options.principal.or_else(|| config.server.principal.clone());
    let embedding = create_embedding_provider_with_clock(&config.embedding, &config.limits, None, Arc::clone(&clock)).await;
    let engine = LocalHoldEngine::new_with_clock(store, embedding, config.limits, config.search, clock);
    let terminal = ratatui::try_init()?;
    let result = event_loop(terminal, engine.clone(), principal).await;
    ratatui::restore();
    engine.shutdown().await;
    result
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
