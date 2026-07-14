//! Application state and update logic for `hold ui`.

use std::fmt;

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    engine::{LocalHoldEngine, SearchRequest},
    store::MemoryStore,
    types::{AuditEntry, Memory, MemoryFilter, MemoryStats, QueryContext, ScopeDefinition, SearchMode},
    ui::theme::Theme,
};

/// Which pane owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// The scope list on the left.
    Scopes,
    /// The memory table on the right.
    Memories,
}

/// Input mode: normal browsing, query editing, or the detail overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Normal list navigation.
    Browse,
    /// The search input owns keystrokes.
    Search,
    /// The detail overlay is open.
    Detail,
}

/// Status line verb + message, in the CLI voice from `assets/brand/cli.md`.
#[derive(Debug)]
pub(crate) enum Status {
    /// Success: something is held.
    Held(String),
    /// Failure: what happened, and ideally how to fix it.
    NotHeld(String),
    /// Neutral note.
    Note(String),
}

/// A memory row, with its composite score when produced by search.
#[derive(Debug)]
pub(crate) struct Row {
    /// The memory backing this row.
    pub memory: Memory,
    /// Composite score (0-100) when this row came from recall.
    pub score: Option<f64>,
}

/// State for the detail overlay.
#[derive(Debug)]
pub(crate) struct Detail {
    /// The memory being inspected.
    pub memory: Memory,
    /// Its audit trail, newest first.
    pub audit: Vec<AuditEntry>,
    /// Vertical scroll offset for long content.
    pub scroll: u16,
}

/// Results delivered back from spawned data tasks.
#[derive(Debug)]
pub(crate) enum DataMsg {
    /// A page of rows from browsing or searching.
    Rows {
        /// The rows to display.
        rows: Vec<Row>,
        /// The search mode the engine actually used; `None` for plain listing.
        mode: Option<SearchMode>,
        /// Generation stamp; stale responses are dropped.
        generation: u64,
    },
    /// A data task failed.
    Failed {
        /// Human-readable failure, shown on the status line.
        message: String,
        /// Generation stamp; stale responses are dropped.
        generation: u64,
    },
}

/// TUI application state. Rendering reads it; `on_event`/`on_data` mutate it.
#[derive(Debug)]
pub(crate) struct App<S>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    /// Engine connected to the configured backend.
    pub engine: LocalHoldEngine<S>,
    /// Sender that spawned data tasks report back through.
    pub data_tx: UnboundedSender<DataMsg>,
    /// Read-visibility principal.
    pub principal: Option<String>,
    /// Stamp for in-flight data tasks; stale responses are dropped.
    pub generation: u64,
    /// Resolved tincture palette.
    pub theme: Theme,
    /// Clock snapshot used for relative ages, refreshed with the data.
    pub now: DateTime<Utc>,
    /// Registered scopes; index 0 in the UI is the synthetic "(all)" entry.
    pub scopes: Vec<ScopeDefinition>,
    /// Selected index into the scope pane (0 = all scopes).
    pub scope_selected: usize,
    /// Rows currently shown in the memory table.
    pub rows: Vec<Row>,
    /// Selected index into `rows`.
    pub row_selected: usize,
    /// Pane focus.
    pub focus: Focus,
    /// Input mode.
    pub mode: Mode,
    /// Current query text (empty = browse listing).
    pub query: String,
    /// Requested search mode; `None` follows the config default.
    pub requested_mode: Option<SearchMode>,
    /// Mode the engine reported for the visible results.
    pub executed_mode: Option<SearchMode>,
    /// Detail overlay, when open.
    pub detail: Option<Detail>,
    /// Aggregate stats for the status line.
    pub stats: Option<MemoryStats>,
    /// Status line content.
    pub status: Status,
    /// True while a data task is in flight.
    pub loading: bool,
    /// Set when the user asks to quit.
    pub quit: bool,
}

impl<S> App<S>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    /// Build the initial state around a connected engine.
    pub(crate) fn new(engine: LocalHoldEngine<S>, theme: Theme, principal: Option<String>, data_tx: UnboundedSender<DataMsg>) -> Self {
        let now = engine.now();
        Self {
            engine,
            data_tx,
            principal,
            generation: 0_u64,
            theme,
            now,
            scopes: Vec::new(),
            scope_selected: 0_usize,
            rows: Vec::new(),
            row_selected: 0_usize,
            focus: Focus::Memories,
            mode: Mode::Browse,
            query: String::new(),
            requested_mode: None,
            executed_mode: None,
            detail: None,
            stats: None,
            status: Status::Note("recalling the hold\u{2026}".into()),
            loading: true,
            quit: false,
        }
    }

    /// Load scopes and stats, then kick off the first listing.
    pub(crate) async fn bootstrap(&mut self) {
        match self.engine.list_scopes().await {
            Ok(scopes) => self.scopes = scopes,
            Err(error) => self.status = Status::NotHeld(format!("scopes unavailable: {error}")),
        }
        match self.engine.count_memories(MemoryFilter::default(), self.ctx(), 0_usize).await {
            Ok(stats) => self.stats = Some(stats),
            Err(error) => self.status = Status::NotHeld(format!("stats unavailable: {error}")),
        }
        self.refresh();
    }

    fn ctx(&self) -> QueryContext {
        QueryContext {
            principal: self.principal.clone(),
        }
    }

    /// The scope key currently filtering the view, if any.
    pub(crate) fn selected_scope(&self) -> Option<String> {
        self.scope_selected
            .checked_sub(1_usize)
            .and_then(|index| self.scopes.get(index))
            .map(|scope| scope.scope_key.clone())
    }

    fn filter(&self, limit: Option<usize>) -> MemoryFilter {
        MemoryFilter {
            scopes_any: self.selected_scope().map(|key| vec![key]),
            limit,
            ..Default::default()
        }
    }

    /// Re-run the visible listing or search under the current filter.
    pub(crate) fn refresh(&mut self) {
        self.generation = self.generation.saturating_add(1_u64);
        self.loading = true;
        self.now = self.engine.now();
        if self.query.is_empty() { self.spawn_list() } else { self.spawn_search() }
    }

    fn spawn_list(&self) {
        let engine = self.engine.clone();
        let tx = self.data_tx.clone();
        let generation = self.generation;
        let filter = self.filter(Some(200_usize));
        let ctx = self.ctx();
        #[expect(unused_results, reason = "JoinHandle intentionally dropped — the result arrives via the data channel")]
        tokio::spawn(async move {
            let msg = match engine.list_memories(filter, ctx).await {
                Ok(memories) => DataMsg::Rows {
                    rows: memories.into_iter().map(|memory| Row { memory, score: None }).collect(),
                    mode: None,
                    generation,
                },
                Err(error) => DataMsg::Failed {
                    message: error.to_string(),
                    generation,
                },
            };
            drop(tx.send(msg));
        });
    }

    fn spawn_search(&self) {
        let engine = self.engine.clone();
        let tx = self.data_tx.clone();
        let generation = self.generation;
        let request = SearchRequest {
            query: self.query.clone(),
            limit: 50_usize,
            filter: self.filter(None),
            ctx: self.ctx(),
            max_distance: None,
            keywords: None,
            search_mode: self.requested_mode,
            context: None,
        };
        #[expect(unused_results, reason = "JoinHandle intentionally dropped — the result arrives via the data channel")]
        tokio::spawn(async move {
            let msg = match engine.search_memories_read_only(request).await {
                Ok(outcome) => {
                    let mode = Some(outcome.search_mode);
                    let rows = outcome
                        .results
                        .into_iter()
                        .map(|result| Row {
                            score: result.composite_score,
                            memory: result.memory,
                        })
                        .collect();
                    DataMsg::Rows { rows, mode, generation }
                }
                Err(error) => DataMsg::Failed {
                    message: error.to_string(),
                    generation,
                },
            };
            drop(tx.send(msg));
        });
    }

    /// Fold a completed data task into the state.
    pub(crate) fn on_data(&mut self, msg: DataMsg) {
        match msg {
            DataMsg::Rows { rows, mode, generation } if generation == self.generation => {
                self.rows = rows;
                self.executed_mode = mode;
                self.loading = false;
                self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
                self.status = self.results_status();
            }
            DataMsg::Failed { message, generation } if generation == self.generation => {
                self.loading = false;
                self.status = Status::NotHeld(message);
            }
            DataMsg::Rows { .. } | DataMsg::Failed { .. } => {}
        }
    }

    fn results_status(&self) -> Status {
        let count = self.rows.len();
        if self.query.is_empty() {
            if count == 0_usize {
                return Status::Note("the hold is empty here \u{2014} remember something".into());
            }
            return Status::Held(format!("{count} memories"));
        }
        let mode = self.executed_mode.map_or_else(String::new, |mode| format!(" ({mode})"));
        if count == 0_usize {
            return Status::Note(format!("nothing found{mode}"));
        }
        Status::Held(format!("{count} results{mode}"))
    }

    /// Route a terminal event.
    pub(crate) async fn on_event(&mut self, event: Event) {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                self.quit = true;
                return;
            }
            match self.mode {
                Mode::Browse => self.key_browse(key).await,
                Mode::Search => self.key_search(key),
                Mode::Detail => self.key_detail(key),
            }
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    async fn key_browse(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => self.focus = if self.focus == Focus::Scopes { Focus::Memories } else { Focus::Scopes },
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(true),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(false),
            KeyCode::Char('g') | KeyCode::Home => self.jump_selection(true),
            KeyCode::Char('G') | KeyCode::End => self.jump_selection(false),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('m') => self.cycle_mode(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.refresh();
                }
            }
            KeyCode::Enter => self.open_or_focus().await,
            _other => {}
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    fn key_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Enter => {
                self.mode = Mode::Browse;
                self.focus = Focus::Memories;
                self.refresh();
            }
            KeyCode::Backspace => {
                let _removed = self.query.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.query.clear(),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => self.query.push(ch),
            _other => {}
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    fn key_detail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.detail = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(detail) = self.detail.as_mut() {
                    detail.scroll = detail.scroll.saturating_add(1_u16);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(detail) = self.detail.as_mut() {
                    detail.scroll = detail.scroll.saturating_sub(1_u16);
                }
            }
            _other => {}
        }
    }

    async fn open_or_focus(&mut self) {
        match self.focus {
            Focus::Scopes => self.focus = Focus::Memories,
            Focus::Memories => self.open_detail().await,
        }
    }

    async fn open_detail(&mut self) {
        let Some(row) = self.rows.get(self.row_selected) else { return };
        let id = row.memory.id;
        match self.engine.get_memory(&id, self.principal.as_deref()).await {
            Ok(Some(memory)) => {
                let mut audit = match self.engine.query_audit_log(&id, 20_usize).await {
                    Ok(entries) => entries,
                    Err(error) => {
                        self.status = Status::NotHeld(format!("history unavailable: {error}"));
                        Vec::new()
                    }
                };
                if memory.was_redacted {
                    redact_audit(&mut audit);
                }
                self.detail = Some(Detail { memory, audit, scroll: 0_u16 });
                self.mode = Mode::Detail;
            }
            Ok(None) => self.status = Status::NotHeld("memory is no longer visible to this principal".into()),
            Err(error) => self.status = Status::NotHeld(format!("read failed: {error}")),
        }
    }

    fn move_selection(&mut self, down: bool) {
        match self.focus {
            Focus::Memories => {
                let last = self.rows.len().saturating_sub(1_usize);
                self.row_selected = if down {
                    self.row_selected.saturating_add(1_usize).min(last)
                } else {
                    self.row_selected.saturating_sub(1_usize)
                };
            }
            Focus::Scopes => {
                let last = self.scopes.len();
                let next = if down {
                    self.scope_selected.saturating_add(1_usize).min(last)
                } else {
                    self.scope_selected.saturating_sub(1_usize)
                };
                if next != self.scope_selected {
                    self.scope_selected = next;
                    self.row_selected = 0_usize;
                    self.refresh();
                }
            }
        }
    }

    fn jump_selection(&mut self, top: bool) {
        if self.focus == Focus::Memories {
            self.row_selected = if top { 0_usize } else { self.rows.len().saturating_sub(1_usize) };
        }
    }

    fn cycle_mode(&mut self) {
        self.requested_mode = match self.requested_mode {
            None => Some(SearchMode::Keyword),
            Some(SearchMode::Keyword) => Some(SearchMode::Text),
            Some(SearchMode::Text) => Some(SearchMode::Semantic),
            Some(SearchMode::Semantic) => Some(SearchMode::Hybrid),
            Some(SearchMode::Hybrid | SearchMode::Auto) => None,
        };
        if !self.query.is_empty() {
            self.refresh();
        }
    }
}

fn redact_audit(audit: &mut [AuditEntry]) {
    for entry in audit {
        entry.caller_agent = None;
        entry.details = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ratatui::{
        Terminal,
        backend::TestBackend,
        crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers},
    };
    use tokio::sync::mpsc;

    use super::{App, DataMsg, Mode, Row, Status};
    use crate::{
        config::{LimitsConfig, SearchConfig},
        embedding::{BoxFuture, EmbeddingProvider},
        engine::LocalHoldEngine,
        error::EmbeddingError,
        store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
        types::{AccessPolicy, Memory, Provenance, RedactableField},
        ui::{theme::Theme, view},
    };

    struct FixedEmbedding;

    impl EmbeddingProvider for FixedEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Ok(vec![1.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS]) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    async fn app_with_memories(contents: &[&str]) -> (App<SqliteStore>, mpsc::UnboundedReceiver<DataMsg>) {
        let store = SqliteStore::in_memory().unwrap();
        for content in contents {
            let memory = Memory::new_for_test((*content).to_owned(), Vec::new(), Provenance::default(), AccessPolicy::Public);
            let _id = store.store(&memory, None).await.unwrap();
        }
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(engine, Theme::detect(), None, tx), rx)
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[tokio::test]
    async fn bootstrap_lists_the_hold() {
        let (mut app, mut rx) = app_with_memories(&["the keep stands", "the gate is open"]).await;
        app.bootstrap().await;
        let msg = rx.recv().await.unwrap();
        app.on_data(msg);
        assert_eq!(app.rows.len(), 2_usize, "both stored memories should be listed");
        assert!(!app.loading, "listing should complete loading");
        assert!(matches!(app.status, Status::Held(_)), "status should report held");
    }

    #[tokio::test]
    async fn search_keys_edit_the_query_and_find_matches() {
        let (mut app, mut rx) = app_with_memories(&["the bastion plan is gold", "unrelated note"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Char('/'))).await;
        assert_eq!(app.mode, Mode::Search, "slash should enter search mode");
        for ch in "bastion".chars() {
            app.on_event(press(KeyCode::Char(ch))).await;
        }
        app.on_event(press(KeyCode::Enter)).await;
        assert_eq!(app.mode, Mode::Browse, "enter should leave search mode");
        app.on_data(rx.recv().await.unwrap());
        assert!(
            app.rows.iter().any(|row| row.memory.content.contains("bastion")),
            "keyword search should surface the matching memory"
        );
        let id = app.rows.iter().find(|row| row.memory.content.contains("bastion")).unwrap().memory.id;
        let stored = app.engine.store().get(&id, None).await.unwrap().unwrap();
        assert_eq!(stored.impression_count, 0_u64, "TUI search must not write analytics impressions");
        assert!(stored.last_impressed_at.is_none(), "TUI search must not write impression timestamps");
    }

    #[tokio::test]
    async fn empty_search_result_is_a_neutral_note() {
        let (mut app, _rx) = app_with_memories(&[]).await;
        app.query = "missing".into();
        app.on_data(DataMsg::Rows {
            rows: Vec::new(),
            mode: Some(crate::types::SearchMode::Text),
            generation: 0_u64,
        });
        assert!(matches!(app.status, Status::Note(_)), "zero matches must not be reported as held");
    }

    #[tokio::test]
    async fn redacted_detail_hides_audit_principals() {
        let store = SqliteStore::in_memory().unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let mut memory = Memory::new_for_test("visible content".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        memory.provenance.source_agent = Some("owner".into());
        memory.access_policy = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        };
        let id = engine.store_memory(memory.clone(), None).await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("outsider".into()), tx);
        app.rows.push(Row { memory, score: None });

        app.open_detail().await;

        assert!(app.detail.is_some(), "redacted memory should remain visible");
        let detail = app.detail.as_ref().unwrap();
        assert!(detail.memory.was_redacted);
        assert!(!detail.audit.is_empty(), "the store audit should be present before sanitization");
        assert!(detail.audit.iter().all(|entry| entry.caller_agent.is_none()));
        assert!(detail.audit.iter().all(|entry| entry.details.is_none()));
        assert_eq!(detail.memory.id, id);
    }

    #[tokio::test]
    async fn stale_generations_are_dropped() {
        let (mut app, mut rx) = app_with_memories(&["the keep stands"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let fresh = app.rows.len();
        app.on_data(DataMsg::Failed {
            message: "late failure".into(),
            generation: 0_u64,
        });
        assert_eq!(app.rows.len(), fresh, "stale responses must not disturb the view");
        assert!(matches!(app.status, Status::Held(_)), "stale failures must not overwrite status");
    }

    #[tokio::test]
    async fn quit_key_sets_quit() {
        let (mut app, _rx) = app_with_memories(&[]).await;
        app.on_event(press(KeyCode::Char('q'))).await;
        assert!(app.quit, "q should request quit");
    }

    #[tokio::test]
    async fn frame_renders_brand_chrome() {
        let (mut app, mut rx) = app_with_memories(&["the keep stands"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let rendered: String = terminal.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(rendered.contains("localhold"), "header should carry the wordmark");
        assert!(rendered.contains("SCOPES"), "scope pane should be titled");
        assert!(rendered.contains("MEMORIES"), "memory pane should be titled");
        assert!(rendered.contains('\u{2580}'), "the battlement rule should be drawn");
        assert!(rendered.contains("held"), "the status line should speak the brand verb");
    }
}
