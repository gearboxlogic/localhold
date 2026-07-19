//! Application state and update logic for `hold ui`.

use std::{collections::HashMap, fmt, future::Future, pin::Pin, sync::Arc};

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::{OnceCell, mpsc::UnboundedSender};

use crate::{
    engine::{LocalHoldEngine, SearchRequest},
    store::MemoryStore,
    types::{AuditEntry, Memory, MemoryFilter, MemoryId, MemoryMetadata, QueryContext, ScopeDefinition, SearchMode, WriteOutcome},
    ui::{
        editor::{EditDraft, ParsedEdit},
        theme::Theme,
    },
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
    /// The detail view is open.
    Detail,
    /// The in-app editor owns keystrokes.
    Edit,
    /// Destructive deletion is awaiting confirmation.
    ConfirmDelete,
    /// A dirty edit is awaiting discard confirmation.
    ConfirmDiscard,
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

/// One visible, non-empty scope in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopeItem {
    /// Exact persisted key used for filtering.
    pub scope_key: String,
    /// Compact human-readable label.
    pub label: String,
    /// Number of visible memories assigned to this exact scope.
    pub count: u64,
}

/// State for the detail overlay.
#[derive(Debug)]
pub(crate) struct Detail {
    /// The memory being inspected.
    pub memory: Memory,
    /// Visible card metadata, when present.
    pub metadata: Option<MemoryMetadata>,
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
    /// Authorized scope facets and optional registry definitions.
    ScopeFacets {
        /// Registered definitions used only to enrich labels.
        definitions: Vec<ScopeDefinition>,
        /// Visible counts by exact persisted scope key.
        by_scope: Vec<(String, u64)>,
        /// Total number of visible memories, including memories without a scope.
        total: u64,
        /// Nonfatal registry-loading warning.
        registry_warning: Option<String>,
        /// Facet generation; stale responses are dropped.
        generation: u64,
    },
    /// Scope facet aggregation failed while row browsing remains available.
    ScopeFacetsFailed {
        /// Human-readable failure.
        message: String,
        /// Facet generation; stale responses are dropped.
        generation: u64,
    },
    /// An edit completed and the refreshed detail is available.
    Updated {
        /// Updated memory.
        memory: Box<Memory>,
        /// Updated metadata, when present.
        metadata: Option<MemoryMetadata>,
        /// Refreshed audit trail.
        audit: Vec<AuditEntry>,
        /// Warning when the mutation committed but a detail refresh failed.
        refresh_warning: Option<String>,
        /// Mutation generation; stale responses are dropped.
        generation: u64,
    },
    /// An edit committed but the memory is no longer visible afterward.
    UpdatedInvisible {
        /// Updated memory ID.
        id: MemoryId,
        /// Mutation generation; stale responses are dropped.
        generation: u64,
    },
    /// An edit committed but its updated memory could not be refreshed.
    UpdatedUnrefreshed {
        /// Updated memory ID.
        id: MemoryId,
        /// Human-readable refresh failure.
        message: String,
        /// Mutation generation; stale responses are dropped.
        generation: u64,
    },
    /// A mutation found that the selected memory no longer exists.
    Missing {
        /// Missing memory ID.
        id: MemoryId,
        /// Mutation generation; stale responses are dropped.
        generation: u64,
    },
    /// A memory was deleted.
    Deleted {
        /// Deleted memory ID.
        id: MemoryId,
        /// Mutation generation; stale responses are dropped.
        generation: u64,
    },
    /// A mutation task failed.
    MutationFailed {
        /// Human-readable failure.
        message: String,
        /// Mutation generation; stale responses are dropped.
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

pub(crate) type MutationEngineFuture<S> = Pin<Box<dyn Future<Output = Result<LocalHoldEngine<S>, String>> + Send>>;
pub(crate) type MutationEngineFactory<S> = Arc<dyn Fn() -> MutationEngineFuture<S> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct LazyMutationEngine<S>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    engine: Arc<OnceCell<LocalHoldEngine<S>>>,
    factory: MutationEngineFactory<S>,
}

impl<S> fmt::Debug for LazyMutationEngine<S>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyMutationEngine")
            .field("initialized", &self.engine.initialized())
            .finish_non_exhaustive()
    }
}

impl<S> LazyMutationEngine<S>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    pub(crate) fn new(factory: MutationEngineFactory<S>) -> Self {
        Self {
            engine: Arc::new(OnceCell::new()),
            factory,
        }
    }

    async fn get(&self) -> Result<&LocalHoldEngine<S>, String> {
        self.engine.get_or_try_init(|| (self.factory)()).await
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(engine) = self.engine.get() {
            engine.shutdown().await;
        }
    }
}

/// TUI application state. Rendering reads it; `on_event`/`on_data` mutate it.
#[derive(Debug)]
pub(crate) struct App<S>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    /// Read-only engine connected to the configured backend.
    pub engine: LocalHoldEngine<S>,
    /// Writable engine acquired and cached on the first explicit mutation.
    pub mutation_engine: LazyMutationEngine<S>,
    /// Sender that spawned data tasks report back through.
    pub data_tx: UnboundedSender<DataMsg>,
    /// Read-visibility principal.
    pub principal: Option<String>,
    /// Stamp for in-flight data tasks; stale responses are dropped.
    pub generation: u64,
    /// Stamp for in-flight scope facet tasks.
    pub facet_generation: u64,
    /// Resolved tincture palette.
    pub theme: Theme,
    /// Clock snapshot used for relative ages, refreshed with the data.
    pub now: DateTime<Utc>,
    /// Visible, non-empty scopes; index 0 in the UI is the synthetic all entry.
    pub scopes: Vec<ScopeItem>,
    /// Total number of memories represented by the all entry, when available.
    pub scope_total: Option<u64>,
    /// Nonfatal scope-loading notice.
    pub scope_notice: Option<String>,
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
    /// Detail view, when open.
    pub detail: Option<Detail>,
    /// Edit draft, while editing or confirming discard.
    pub edit: Option<EditDraft>,
    /// Stamp for in-flight mutations; stale responses are dropped.
    pub operation_generation: u64,
    /// True while an edit or delete is in flight.
    pub pending: bool,
    /// Persistent operator notice displayed beside transient status.
    pub notice: Option<String>,
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
    /// Build state around one engine for focused state-machine tests.
    #[cfg(test)]
    pub(crate) fn new(engine: LocalHoldEngine<S>, theme: Theme, principal: Option<String>, data_tx: UnboundedSender<DataMsg>) -> Self {
        let mutation_engine = engine.clone();
        let factory: MutationEngineFactory<S> = Arc::new(move || {
            let engine = mutation_engine.clone();
            Box::pin(async move { Ok(engine) })
        });
        Self::new_with_mutation_factory(engine, theme, principal, data_tx, factory)
    }

    /// Build state with a writable engine that is opened only on first mutation.
    pub(crate) fn new_with_mutation_factory(
        engine: LocalHoldEngine<S>,
        theme: Theme,
        principal: Option<String>,
        data_tx: UnboundedSender<DataMsg>,
        mutation_factory: MutationEngineFactory<S>,
    ) -> Self {
        let now = engine.now();
        Self {
            engine,
            mutation_engine: LazyMutationEngine::new(mutation_factory),
            data_tx,
            principal,
            generation: 0_u64,
            facet_generation: 0_u64,
            theme,
            now,
            scopes: Vec::new(),
            scope_total: None,
            scope_notice: None,
            scope_selected: 0_usize,
            rows: Vec::new(),
            row_selected: 0_usize,
            focus: Focus::Memories,
            mode: Mode::Browse,
            query: String::new(),
            requested_mode: None,
            executed_mode: None,
            detail: None,
            edit: None,
            operation_generation: 0_u64,
            pending: false,
            notice: None,
            status: Status::Note("recalling the hold\u{2026}".into()),
            loading: true,
            quit: false,
        }
    }

    /// Drain background work owned by the lazily opened mutation engine.
    pub(crate) async fn shutdown_mutation_engine(&self) {
        self.mutation_engine.shutdown().await;
    }

    /// Kick off the first authorized scope facet and bounded memory listing.
    pub(crate) async fn bootstrap(&mut self) {
        let (definitions, stats) = tokio::join!(self.engine.list_scopes(), self.engine.count_memories(MemoryFilter::default(), self.ctx(), 0_usize),);
        match stats {
            Ok(stats) => {
                let (definitions, registry_warning) = match definitions {
                    Ok(definitions) => (definitions, None),
                    Err(error) => (Vec::new(), Some(format!("scope names unavailable; showing keys: {error}"))),
                };
                self.apply_scope_facets(definitions, stats.by_scope, stats.total, registry_warning);
            }
            Err(error) => {
                self.scope_notice = Some(format!("scope counts unavailable: {error}"));
            }
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

    fn refresh_all(&mut self) {
        self.refresh_scope_facets();
        self.refresh();
    }

    fn refresh_scope_facets(&mut self) {
        self.facet_generation = self.facet_generation.saturating_add(1_u64);
        let generation = self.facet_generation;
        let engine = self.engine.clone();
        let tx = self.data_tx.clone();
        let ctx = self.ctx();
        #[expect(unused_results, reason = "JoinHandle intentionally dropped — the result arrives via the data channel")]
        tokio::spawn(async move {
            let (definitions, stats) = tokio::join!(engine.list_scopes(), engine.count_memories(MemoryFilter::default(), ctx, 0_usize),);
            let msg = match stats {
                Ok(stats) => {
                    let (definitions, registry_warning) = match definitions {
                        Ok(definitions) => (definitions, None),
                        Err(error) => (Vec::new(), Some(format!("scope names unavailable; showing keys: {error}"))),
                    };
                    DataMsg::ScopeFacets {
                        definitions,
                        by_scope: stats.by_scope,
                        total: stats.total,
                        registry_warning,
                        generation,
                    }
                }
                Err(error) => DataMsg::ScopeFacetsFailed {
                    message: format!("scope counts unavailable: {error}"),
                    generation,
                },
            };
            drop(tx.send(msg));
        });
    }

    fn apply_scope_facets(&mut self, definitions: Vec<ScopeDefinition>, by_scope: Vec<(String, u64)>, total: u64, registry_warning: Option<String>) {
        let selected = self.selected_scope();
        self.scopes = build_scope_items(definitions, by_scope);
        self.scope_total = Some(total);
        self.scope_notice = registry_warning;
        self.scope_selected = selected
            .as_ref()
            .and_then(|key| self.scopes.iter().position(|scope| &scope.scope_key == key))
            .map_or(0_usize, |index| index.saturating_add(1_usize));
        if selected.is_some() && self.scope_selected == 0_usize {
            self.row_selected = 0_usize;
            self.refresh();
        }
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

    /// Fold a completed data or mutation task into the state.
    #[expect(clippy::too_many_lines, reason = "the reducer keeps all generation-checked data transitions together")]
    pub(crate) fn on_data(&mut self, msg: DataMsg) {
        match msg {
            DataMsg::ScopeFacets {
                definitions,
                by_scope,
                total,
                registry_warning,
                generation,
            } if generation == self.facet_generation => {
                self.apply_scope_facets(definitions, by_scope, total, registry_warning);
            }
            DataMsg::ScopeFacetsFailed { message, generation } if generation == self.facet_generation => {
                self.scope_notice = Some(message);
            }
            DataMsg::Rows { rows, mode, generation } if generation == self.generation => {
                self.rows = rows.into_iter().map(sanitize_row_for_view).collect();
                self.executed_mode = mode;
                self.loading = false;
                self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
                self.status = self.results_status();
            }
            DataMsg::Failed { message, generation } if generation == self.generation => {
                self.executed_mode = None;
                self.loading = false;
                self.status = Status::NotHeld(message);
            }
            DataMsg::Updated {
                memory,
                metadata,
                mut audit,
                refresh_warning,
                generation,
            } if generation == self.operation_generation => {
                self.pending = false;
                let refresh_results = !self.query.is_empty();
                let memory = (*memory).sanitize_for_wire();
                let metadata = if memory.was_redacted { None } else { metadata };
                if memory.was_redacted {
                    redact_audit(&mut audit);
                }
                if let Some(row) = self.rows.iter_mut().find(|row| row.memory.id == memory.id) {
                    row.memory = memory.clone();
                    row.score = None;
                }
                self.detail = Some(Detail {
                    memory,
                    metadata,
                    audit,
                    scroll: 0_u16,
                });
                self.edit = None;
                self.mode = Mode::Detail;
                self.status = refresh_warning.map_or_else(
                    || Status::Held("memory revised".into()),
                    |warning| Status::NotHeld(format!("memory revised, but {warning}")),
                );
                if refresh_results {
                    self.refresh();
                }
            }
            DataMsg::UpdatedInvisible { id, generation } if generation == self.operation_generation => {
                self.pending = false;
                self.rows.retain(|row| row.memory.id != id);
                self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
                self.detail = None;
                self.edit = None;
                self.mode = Mode::Browse;
                self.status = Status::Held("memory revised and is no longer visible".into());
                self.refresh_scope_facets();
            }
            DataMsg::UpdatedUnrefreshed { id, message, generation } if generation == self.operation_generation => {
                self.pending = false;
                self.rows.retain(|row| row.memory.id != id);
                self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
                self.detail = None;
                self.edit = None;
                self.mode = Mode::Browse;
                self.status = Status::NotHeld(format!("memory revised, but refresh failed: {message}"));
            }
            DataMsg::Missing { id, generation } if generation == self.operation_generation => {
                self.pending = false;
                self.rows.retain(|row| row.memory.id != id);
                self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
                self.detail = None;
                self.edit = None;
                self.mode = Mode::Browse;
                self.status = Status::NotHeld("memory no longer exists".into());
                self.refresh_scope_facets();
            }
            DataMsg::Deleted { id, generation } if generation == self.operation_generation => {
                self.pending = false;
                self.rows.retain(|row| row.memory.id != id);
                self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
                self.detail = None;
                self.edit = None;
                self.mode = Mode::Browse;
                self.status = Status::Held("memory forgotten".into());
                self.refresh_scope_facets();
            }
            DataMsg::MutationFailed { message, generation } if generation == self.operation_generation => {
                self.pending = false;
                self.status = Status::NotHeld(message);
                if self.mode == Mode::ConfirmDelete {
                    self.mode = Mode::Detail;
                }
            }
            DataMsg::ScopeFacets { .. }
            | DataMsg::ScopeFacetsFailed { .. }
            | DataMsg::Rows { .. }
            | DataMsg::Failed { .. }
            | DataMsg::Updated { .. }
            | DataMsg::UpdatedInvisible { .. }
            | DataMsg::UpdatedUnrefreshed { .. }
            | DataMsg::Missing { .. }
            | DataMsg::Deleted { .. }
            | DataMsg::MutationFailed { .. } => {}
        }
    }

    fn results_status(&self) -> Status {
        let count = self.rows.len();
        let scope = self.selected_scope().map_or_else(String::new, |key| format!(" · scope {key}"));
        if self.query.is_empty() {
            return browse_results_status(count, &scope);
        }
        let mode = self.executed_mode.map_or_else(String::new, |mode| format!(" ({mode})"));
        if count == 0_usize {
            return Status::Note(format!("nothing found{mode}{scope}"));
        }
        Status::Held(format!("{count} results{mode}{scope}"))
    }

    fn request_quit(&mut self) {
        if self.pending {
            self.status = Status::Note("waiting for the pending memory change to finish".into());
        } else {
            self.quit = true;
        }
    }

    /// Route a terminal event.
    pub(crate) async fn on_event(&mut self, event: Event) {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                self.request_quit();
                return;
            }
            match self.mode {
                Mode::Browse => self.key_browse(key).await,
                Mode::Search => self.key_search(key),
                Mode::Detail => self.key_detail(key),
                Mode::Edit => self.key_edit(key),
                Mode::ConfirmDelete => self.key_confirm_delete(key),
                Mode::ConfirmDiscard => self.key_confirm_discard(key),
            }
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    async fn key_browse(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab | KeyCode::BackTab => self.focus = if self.focus == Focus::Scopes { Focus::Memories } else { Focus::Scopes },
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Scopes,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Memories,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(true),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(false),
            KeyCode::Char('g') | KeyCode::Home => self.jump_selection(true),
            KeyCode::Char('G') | KeyCode::End => self.jump_selection(false),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('m') => self.cycle_mode(),
            KeyCode::Char('r') => self.refresh_all(),
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
            KeyCode::Char('e') => self.begin_edit(),
            KeyCode::Char('d') => self.begin_delete(),
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

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    fn key_edit(&mut self, key: KeyEvent) {
        if self.pending {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.spawn_save();
            return;
        }
        let Some(edit) = self.edit.as_mut() else {
            self.mode = Mode::Detail;
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = if edit.dirty() { Mode::ConfirmDiscard } else { Mode::Detail };
                if self.mode == Mode::Detail {
                    self.edit = None;
                }
            }
            KeyCode::Enter if edit.field.multiline() => edit.active_mut().insert('\n'),
            KeyCode::Backspace => edit.active_mut().backspace(),
            KeyCode::Delete => edit.active_mut().delete(),
            KeyCode::Left => edit.active_mut().left(),
            KeyCode::Right => edit.active_mut().right(),
            KeyCode::Home => edit.active_mut().home(),
            KeyCode::End => edit.active_mut().end(),
            KeyCode::Up if edit.field.multiline() => edit.active_mut().up(),
            KeyCode::Down if edit.field.multiline() => edit.active_mut().down(),
            KeyCode::BackTab | KeyCode::Up => edit.field = edit.field.next(true),
            KeyCode::Tab | KeyCode::Enter | KeyCode::Down => edit.field = edit.field.next(false),
            KeyCode::Char(ch) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => edit.active_mut().insert(ch),
            _other => {}
        }
        if let Some(edit) = self.edit.as_mut() {
            edit.ensure_cursor_visible();
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    fn key_confirm_delete(&mut self, key: KeyEvent) {
        if self.pending {
            return;
        }
        match key.code {
            KeyCode::Char('y') => self.spawn_delete(),
            KeyCode::Char('n') | KeyCode::Esc => self.mode = Mode::Detail,
            _other => {}
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    fn key_confirm_discard(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') => {
                self.edit = None;
                self.mode = Mode::Detail;
            }
            KeyCode::Char('n') | KeyCode::Esc => self.mode = Mode::Edit,
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
                let memory = memory.sanitize_for_wire();
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
                let metadata = if memory.was_redacted { None } else { self.load_detail_metadata(&id).await };
                self.detail = Some(Detail {
                    memory,
                    metadata,
                    audit,
                    scroll: 0_u16,
                });
                self.mode = Mode::Detail;
            }
            Ok(None) => self.status = Status::NotHeld("memory is no longer visible to this principal".into()),
            Err(error) => self.status = Status::NotHeld(format!("read failed: {error}")),
        }
    }

    async fn load_detail_metadata(&mut self, id: &MemoryId) -> Option<MemoryMetadata> {
        match self.engine.get_metadata(id).await {
            Ok(metadata) => metadata,
            Err(error) => {
                self.status = Status::NotHeld(format!("metadata unavailable: {error}"));
                None
            }
        }
    }

    fn begin_edit(&mut self) {
        if self.pending {
            return;
        }
        let Some(principal) = self.principal.as_deref() else {
            self.status = Status::NotHeld("editing requires --principal or server.principal".into());
            return;
        };
        let Some(detail) = self.detail.as_ref() else { return };
        if detail.memory.was_redacted || !detail.memory.has_write_access(principal) {
            self.status = Status::NotHeld("this principal cannot modify the selected memory".into());
            return;
        }
        self.edit = Some(EditDraft::new(&detail.memory, detail.metadata.as_ref()));
        self.mode = Mode::Edit;
        self.status = Status::Note("editing memory".into());
    }

    fn begin_delete(&mut self) {
        if self.pending {
            return;
        }
        let Some(principal) = self.principal.as_deref() else {
            self.status = Status::NotHeld("deletion requires --principal or server.principal".into());
            return;
        };
        let Some(detail) = self.detail.as_ref() else { return };
        if detail.memory.was_redacted || !detail.memory.has_write_access(principal) {
            self.status = Status::NotHeld("this principal cannot delete the selected memory".into());
            return;
        }
        self.mode = Mode::ConfirmDelete;
    }

    fn spawn_save(&mut self) {
        if self.pending {
            return;
        }
        let Some(principal) = self.principal.clone() else {
            self.status = Status::NotHeld("editing requires --principal or server.principal".into());
            return;
        };
        let Some(detail) = self.detail.as_ref() else { return };
        let Some(edit) = self.edit.as_mut() else { return };
        let ParsedEdit { update, metadata_patch } = match edit.parse() {
            Ok(parsed) => parsed,
            Err(error) => {
                edit.field = error.field;
                self.status = Status::NotHeld(error.message);
                return;
            }
        };
        let parsed = ParsedEdit { update, metadata_patch };
        if parsed.is_empty() {
            self.edit = None;
            self.mode = Mode::Detail;
            self.status = Status::Note("no changes to hold".into());
            return;
        }

        self.operation_generation = self.operation_generation.saturating_add(1_u64);
        let generation = self.operation_generation;
        self.generation = self.generation.saturating_add(1_u64);
        self.loading = false;
        self.pending = true;
        self.status = Status::Note("holding revision\u{2026}".into());
        let mutation_engine = self.mutation_engine.clone();
        let tx = self.data_tx.clone();
        let id = detail.memory.id;
        let expected_revision = detail.memory.record_revision;
        #[expect(unused_results, reason = "JoinHandle intentionally dropped — the result arrives via the data channel")]
        tokio::spawn(async move {
            let msg = match mutation_engine.get().await {
                Ok(engine) => match engine
                    .update_memory_if_unmodified_with_metadata(id, expected_revision, parsed.update, parsed.metadata_patch, &principal)
                    .await
                {
                    Ok(outcome) => match outcome.outcome {
                        WriteOutcome::Applied => refresh_updated_detail(engine, id, &principal, generation).await,
                        WriteOutcome::NotFound => DataMsg::Missing { id, generation },
                        WriteOutcome::Denied => DataMsg::MutationFailed {
                            message: "this principal cannot modify the selected memory".into(),
                            generation,
                        },
                    },
                    Err(error) => DataMsg::MutationFailed {
                        message: error.to_string(),
                        generation,
                    },
                },
                Err(message) => DataMsg::MutationFailed { message, generation },
            };
            drop(tx.send(msg));
        });
    }

    fn spawn_delete(&mut self) {
        if self.pending {
            return;
        }
        let Some(principal) = self.principal.clone() else {
            self.status = Status::NotHeld("deletion requires --principal or server.principal".into());
            return;
        };
        let Some(detail) = self.detail.as_ref() else { return };
        self.operation_generation = self.operation_generation.saturating_add(1_u64);
        let generation = self.operation_generation;
        self.generation = self.generation.saturating_add(1_u64);
        self.loading = false;
        self.pending = true;
        self.status = Status::Note("forgetting memory\u{2026}".into());
        let mutation_engine = self.mutation_engine.clone();
        let tx = self.data_tx.clone();
        let id = detail.memory.id;
        let expected_revision = detail.memory.record_revision;
        #[expect(unused_results, reason = "JoinHandle intentionally dropped — the result arrives via the data channel")]
        tokio::spawn(async move {
            let msg = match mutation_engine.get().await {
                Ok(engine) => match engine.delete_memory_if_unmodified(&id, expected_revision, &principal).await {
                    Ok(WriteOutcome::Applied) => DataMsg::Deleted { id, generation },
                    Ok(WriteOutcome::NotFound) => DataMsg::Missing { id, generation },
                    Ok(WriteOutcome::Denied) => DataMsg::MutationFailed {
                        message: "this principal cannot delete the selected memory".into(),
                        generation,
                    },
                    Err(error) => DataMsg::MutationFailed {
                        message: error.to_string(),
                        generation,
                    },
                },
                Err(message) => DataMsg::MutationFailed { message, generation },
            };
            drop(tx.send(msg));
        });
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
        match self.focus {
            Focus::Memories => {
                self.row_selected = if top { 0_usize } else { self.rows.len().saturating_sub(1_usize) };
            }
            Focus::Scopes => {
                let next = if top { 0_usize } else { self.scopes.len() };
                if next != self.scope_selected {
                    self.scope_selected = next;
                    self.row_selected = 0_usize;
                    self.refresh();
                }
            }
        }
    }

    fn cycle_mode(&mut self) {
        self.requested_mode = match self.requested_mode {
            None => Some(SearchMode::Keyword),
            Some(SearchMode::Keyword) => Some(SearchMode::Text),
            Some(SearchMode::Text) => Some(SearchMode::Semantic),
            Some(SearchMode::Semantic) => Some(SearchMode::Hybrid),
            Some(SearchMode::Hybrid) => Some(SearchMode::Auto),
            Some(SearchMode::Auto) => None,
        };
        if !self.query.is_empty() {
            self.refresh();
        }
    }
}
fn browse_results_status(count: usize, scope: &str) -> Status {
    if count > 0_usize {
        return Status::Held(format!("{count} memories{scope}"));
    }
    if scope.is_empty() {
        return Status::Note("the hold is empty here \u{2014} remember something".into());
    }
    Status::Note(format!("nothing held{scope}"))
}

fn build_scope_items(definitions: Vec<ScopeDefinition>, by_scope: Vec<(String, u64)>) -> Vec<ScopeItem> {
    let display_names = definitions
        .into_iter()
        .filter_map(|definition| {
            let display_name = definition.display_name.trim();
            (!display_name.is_empty()).then(|| (definition.scope_key, display_name.to_owned()))
        })
        .collect::<HashMap<_, _>>();
    let keys = by_scope.iter().filter(|(_, count)| *count > 0_u64).map(|(scope, _)| scope.clone()).collect::<Vec<_>>();
    let raw_keys = keys.iter().filter(|scope| !display_names.contains_key(*scope)).map(String::as_str).collect::<Vec<_>>();

    let mut items = by_scope
        .into_iter()
        .filter(|(_, count)| *count > 0_u64)
        .map(|(scope_key, count)| {
            let label = display_names.get(&scope_key).cloned().unwrap_or_else(|| shortest_unique_scope_label(&scope_key, &raw_keys));
            ScopeItem { scope_key, label, count }
        })
        .collect::<Vec<_>>();

    let mut label_counts = HashMap::new();
    for item in &items {
        let count = label_counts.entry(item.label.to_lowercase()).or_insert(0_usize);
        *count = count.saturating_add(1_usize);
    }
    let all_keys = keys.iter().map(String::as_str).collect::<Vec<_>>();
    for item in &mut items {
        if label_counts.get(&item.label.to_lowercase()).copied().unwrap_or_default() > 1_usize {
            let suffix = shortest_unique_scope_label(&item.scope_key, &all_keys);
            item.label = format!("{} · {suffix}", item.label);
        }
    }
    items.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.scope_key.cmp(&right.scope_key))
    });
    items
}

fn shortest_unique_scope_label(scope: &str, peers: &[&str]) -> String {
    let segments = scope.split(['/', '\\']).filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
    for depth in 1_usize..=segments.len() {
        let candidate = segments[segments.len().saturating_sub(depth)..].join("/");
        let matches = peers
            .iter()
            .filter(|peer| {
                let peer_segments = peer.split(['/', '\\']).filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
                peer_segments.len() >= depth && peer_segments[peer_segments.len().saturating_sub(depth)..].join("/").eq_ignore_ascii_case(&candidate)
            })
            .count();
        if matches == 1_usize {
            return candidate;
        }
    }
    scope.to_owned()
}

async fn refresh_updated_detail<S>(engine: &LocalHoldEngine<S>, id: MemoryId, principal: &str, generation: u64) -> DataMsg
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    match engine.get_memory(&id, Some(principal)).await {
        Ok(Some(memory)) => {
            let (metadata, metadata_warning) = match engine.get_metadata(&id).await {
                Ok(metadata) => (metadata, None),
                Err(error) => (None, Some(format!("metadata refresh failed: {error}"))),
            };
            let (audit, audit_warning) = match engine.query_audit_log(&id, 20_usize).await {
                Ok(audit) => (audit, None),
                Err(error) => (Vec::new(), Some(format!("history refresh failed: {error}"))),
            };
            let refresh_warning = [metadata_warning, audit_warning].into_iter().flatten().collect::<Vec<_>>().join("; ");
            DataMsg::Updated {
                memory: Box::new(memory),
                metadata,
                audit,
                refresh_warning: (!refresh_warning.is_empty()).then_some(refresh_warning),
                generation,
            }
        }
        Ok(None) => DataMsg::UpdatedInvisible { id, generation },
        Err(error) => DataMsg::UpdatedUnrefreshed {
            id,
            message: error.to_string(),
            generation,
        },
    }
}

fn sanitize_row_for_view(row: Row) -> Row {
    let memory = row.memory.sanitize_for_wire();
    let score = row.score.filter(|_| !memory.was_redacted);
    Row { memory, score }
}

fn redact_audit(audit: &mut [AuditEntry]) {
    for entry in audit {
        entry.caller_agent = None;
        entry.details = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers},
        style::{Color, Modifier},
    };
    use tokio::sync::mpsc;

    use super::{App, DataMsg, Focus, Mode, MutationEngineFactory, Row, Status, build_scope_items};
    use crate::{
        config::{LimitsConfig, SearchConfig},
        embedding::{BoxFuture, EmbeddingProvider},
        engine::LocalHoldEngine,
        error::EmbeddingError,
        store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
        types::{AccessPolicy, Memory, MemoryUpdate, Provenance, RedactableField, ScopeDefinition, WriteOutcome},
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

    struct FailingEmbedding;

    impl EmbeddingProvider for FailingEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Err(EmbeddingError::Permanent(std::io::Error::other("provider rejected test input").into())) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct CountingEmbedding {
        calls: Arc<AtomicUsize>,
    }

    impl EmbeddingProvider for CountingEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            let _previous = self.calls.fetch_add(1_usize, Ordering::SeqCst);
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

    fn press_with(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn rendered_text(buffer: &Buffer) -> String {
        buffer.content().iter().map(ratatui::buffer::Cell::symbol).collect()
    }

    fn find_text_start(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0_u16..buffer.area.height {
            let row = (0_u16..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            if let Some(byte_start) = row.find(needle) {
                let x = u16::try_from(row.get(..byte_start)?.chars().count()).ok()?;
                return Some((x, y));
            }
        }
        None
    }

    fn assert_text_color(buffer: &Buffer, text: &str, expected: Color) {
        let position = find_text_start(buffer, text);
        assert!(position.is_some(), "{text:?} was not rendered");
        let (start_x, y) = position.unwrap();
        for (offset, _) in text.chars().enumerate() {
            let x = start_x.saturating_add(u16::try_from(offset).unwrap());
            assert_eq!(buffer[(x, y)].fg, expected, "unexpected color at ({x}, {y}) in {text:?}");
        }
    }

    fn assert_gold_is_battlement_only(buffer: &Buffer, gold: Color) {
        let rule_y = buffer.area.height.saturating_sub(2_u16);
        let gold_positions = (0_u16..buffer.area.height)
            .flat_map(|y| (0_u16..buffer.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| buffer[(x, y)].fg == gold)
            .collect::<Vec<_>>();
        assert!(!gold_positions.is_empty(), "the battlement rule should retain the gold accent");
        assert!(
            gold_positions.iter().all(|&(_, y)| y == rule_y),
            "gold must be confined to the battlement row, found {gold_positions:?}"
        );
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
    async fn post_save_refresh_warning_is_visible() {
        let (mut app, mut rx) = app_with_memories(&["revised"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let memory = app.rows[0].memory.clone();
        app.pending = true;

        app.on_data(DataMsg::Updated {
            memory: Box::new(memory),
            metadata: None,
            audit: Vec::new(),
            refresh_warning: Some("metadata refresh failed: test fault".into()),
            generation: app.operation_generation,
        });

        assert!(!app.pending);
        assert!(
            matches!(&app.status, Status::NotHeld(message) if message.contains("memory revised, but metadata refresh failed")),
            "a refresh failure should be visible"
        );
    }

    #[tokio::test]
    async fn committed_save_with_primary_refresh_failure_closes_dirty_editor() {
        let (mut app, mut rx) = app_with_memories(&["revised"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let id = app.rows[0].memory.id;
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().content.value = "committed".into();
        app.pending = true;

        app.on_data(DataMsg::UpdatedUnrefreshed {
            id,
            message: "test fault".into(),
            generation: app.operation_generation,
        });

        assert!(!app.pending);
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.edit.is_none());
        assert!(app.detail.is_none());
        assert!(!app.rows.iter().any(|row| row.memory.id == id));
        assert!(matches!(&app.status, Status::NotHeld(message) if message.contains("memory revised, but refresh failed")));
    }

    #[tokio::test]
    async fn redacted_rows_hide_composite_scores() {
        let (mut app, _rx) = app_with_memories(&[]).await;
        let mut memory = Memory::new_for_test("[redacted]".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        memory.was_redacted = true;

        app.on_data(DataMsg::Rows {
            rows: vec![Row { memory, score: Some(98.0_f64) }],
            mode: Some(crate::types::SearchMode::Text),
            generation: 0_u64,
        });

        assert_eq!(app.rows.len(), 1_usize);
        assert!(app.rows[0].memory.was_redacted);
        assert!(app.rows[0].score.is_none(), "redacted ranking diagnostics must not reach the view");
    }

    #[tokio::test]
    async fn redacted_detail_hides_audit_principals() {
        let store = SqliteStore::in_memory().unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let mut memory = Memory::new_for_test("visible content".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        memory.provenance.source_agent = Some("owner".into());
        memory.updated_at += chrono::Duration::days(7_i64);
        memory.confidence = crate::types::Confidence::new(0.2_f64);
        memory.superseded_by = Some(crate::types::MemoryId::new());
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
        assert_eq!(detail.memory.updated_at, detail.memory.created_at);
        assert_eq!(detail.memory.confidence, crate::types::Confidence::DEFAULT);
        assert!(detail.memory.superseded_by.is_none());
        assert_eq!(detail.memory.id, id);
    }

    #[tokio::test]
    async fn filtered_results_refresh_after_an_edit_stops_matching() {
        let (mut app, mut rx) = app_with_memories(&["needle original", "unrelated"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.query = "needle".into();
        app.requested_mode = Some(crate::types::SearchMode::Keyword);
        app.refresh();
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.rows.len(), 1_usize);
        let id = app.rows[0].memory.id;

        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().content.value = "no longer matches".into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert!(app.loading, "a successful filtered edit should re-run the active search");
        app.on_data(rx.recv().await.unwrap());
        assert!(!app.rows.iter().any(|row| row.memory.id == id));
    }

    #[tokio::test]
    async fn save_not_found_removes_stale_editor_state() {
        let (mut app, mut rx) = app_with_memories(&["deleted elsewhere"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let id = app.rows[0].memory.id;
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().content.value = "stale edit".into();
        assert!(app.engine.store().delete(&id).await.unwrap());

        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(app.mode, Mode::Browse);
        assert!(app.edit.is_none());
        assert!(app.detail.is_none());
        assert!(!app.rows.iter().any(|row| row.memory.id == id));
        assert!(matches!(&app.status, Status::NotHeld(message) if message == "memory no longer exists"));
    }

    #[tokio::test]
    async fn pre_mutation_rows_cannot_resurrect_a_deleted_memory() {
        let (mut app, mut rx) = app_with_memories(&["stale row"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let stale_rows = vec![Row {
            memory: app.rows[0].memory.clone(),
            score: app.rows[0].score,
        }];
        let id = stale_rows[0].memory.id;
        app.on_event(press(KeyCode::Enter)).await;
        app.refresh();
        let stale_generation = app.generation;

        app.on_event(press(KeyCode::Char('d'))).await;
        app.on_event(press(KeyCode::Char('y'))).await;
        assert!(app.generation > stale_generation, "starting a mutation must invalidate in-flight reads");
        app.on_data(DataMsg::Deleted {
            id,
            generation: app.operation_generation,
        });
        app.on_data(DataMsg::Rows {
            rows: stale_rows,
            mode: None,
            generation: stale_generation,
        });

        assert!(!app.rows.iter().any(|row| row.memory.id == id));
        assert_eq!(app.mode, Mode::Browse);
    }

    #[tokio::test]
    async fn stale_generations_are_dropped() {
        let (mut app, mut rx) = app_with_memories(&["the keep stands"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());

        let generation = app.generation;
        let rows = std::mem::take(&mut app.rows);
        app.on_data(DataMsg::Rows {
            rows,
            mode: Some(crate::types::SearchMode::Text),
            generation,
        });
        let fresh = app.rows.len();
        app.loading = true;
        app.requested_mode = Some(crate::types::SearchMode::Text);
        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode text"));

        app.on_data(DataMsg::Failed {
            message: "late failure".into(),
            generation: 0_u64,
        });
        assert_eq!(app.rows.len(), fresh, "stale responses must not disturb the view");
        assert!(matches!(app.status, Status::Held(_)), "stale failures must not overwrite status");
        assert_eq!(
            app.executed_mode,
            Some(crate::types::SearchMode::Text),
            "stale failures must not clear the last executed mode"
        );
        assert!(app.loading, "stale failures must not finish the current refresh");
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode text"));

        app.requested_mode = Some(crate::types::SearchMode::Hybrid);
        app.on_data(DataMsg::Failed {
            message: "current failure".into(),
            generation,
        });
        assert_eq!(app.executed_mode, None, "current failures must clear the old executed mode");
        assert!(!app.loading, "current failures must finish the refresh");
        assert!(matches!(&app.status, Status::NotHeld(message) if message == "current failure"));
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode hybrid"));
    }

    #[tokio::test]
    async fn quit_key_sets_quit() {
        let (mut app, _rx) = app_with_memories(&[]).await;
        app.on_event(press(KeyCode::Char('q'))).await;
        assert!(app.quit, "q should request quit");
    }

    #[tokio::test]
    async fn ctrl_c_waits_for_pending_mutation() {
        let (mut app, _rx) = app_with_memories(&[]).await;
        app.pending = true;

        app.on_event(press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)).await;

        assert!(!app.quit);
        assert!(matches!(&app.status, Status::Note(message) if message.contains("pending memory change")));

        app.pending = false;
        app.on_event(press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)).await;
        assert!(app.quit);
    }

    #[tokio::test]
    async fn mode_cycle_includes_explicit_auto_before_config_default() {
        let (mut app, _rx) = app_with_memories(&[]).await;
        let expected = [
            Some(crate::types::SearchMode::Keyword),
            Some(crate::types::SearchMode::Text),
            Some(crate::types::SearchMode::Semantic),
            Some(crate::types::SearchMode::Hybrid),
            Some(crate::types::SearchMode::Auto),
            None,
        ];

        for mode in expected {
            app.cycle_mode();
            assert_eq!(app.requested_mode, mode);
        }
    }

    #[tokio::test]
    async fn detail_edit_saves_fields_metadata_and_queues_embedding_after_commit() {
        let (mut app, mut rx) = app_with_memories(&["original memory"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let id = app.rows[0].memory.id;

        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        let edit = app.edit.as_mut().unwrap();
        edit.content.value = "revised memory".into();
        edit.tags.value = r#"["alpha","beta"]"#.into();
        edit.importance.value = "0.90".into();
        edit.expiry.value = "2026-08-01T12:00:00Z".into();
        edit.metadata.value = r#"{"summary":"revised summary","agent_label":"operator"}"#.into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        assert!(app.pending);

        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.mode, Mode::Detail);
        assert!(!app.pending);
        app.shutdown_mutation_engine().await;
        let stored = app.engine.store().get(&id, Some("operator")).await.unwrap().unwrap();
        assert_eq!(stored.content, "revised memory");
        assert_eq!(stored.tags, vec!["alpha", "beta"]);
        assert_eq!(stored.importance, crate::types::Importance::new(0.9_f64));
        assert!(stored.expires_at.is_some());
        assert!(stored.has_embedding);
        assert!(app.engine.store().fetch_embeddings_for_ids(&[id]).await.unwrap().contains_key(&id));
        let metadata = app.engine.get_metadata(&id).await.unwrap().unwrap();
        assert_eq!(metadata.summary.as_deref(), Some("revised summary"));
        assert_eq!(metadata.agent_label.as_deref(), Some("operator"));
    }

    #[tokio::test]
    async fn browsing_does_not_initialize_the_mutation_engine() {
        let store = SqliteStore::in_memory().unwrap();
        let memory = Memory::new_for_test("lazy writer".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        let _id = store.store(&memory, None).await.unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let writer_engine = engine.clone();
        let opens = Arc::new(AtomicUsize::new(0_usize));
        let factory_opens = Arc::clone(&opens);
        let factory: MutationEngineFactory<SqliteStore> = Arc::new(move || {
            let engine = writer_engine.clone();
            let _previous = factory_opens.fetch_add(1_usize, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(engine)))
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new_with_mutation_factory(engine, Theme::detect(), Some("operator".into()), tx, factory);

        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;
        assert_eq!(opens.load(Ordering::SeqCst), 0_usize, "browsing and opening detail must stay read-only");

        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().tags.value = r#"["revised"]"#.into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(opens.load(Ordering::SeqCst), 1_usize, "the first explicit save should acquire one writable engine");
    }

    #[tokio::test]
    async fn embedding_failure_saves_content_without_a_stale_vector() {
        let store = SqliteStore::in_memory().unwrap();
        let memory = Memory::new_for_test("original".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        let id = store.store(&memory, None).await.unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FailingEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().content.value = "rejected revision".into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(app.mode, Mode::Detail);
        assert!(app.edit.is_none());
        assert!(matches!(app.status, Status::Held(_)));
        let stored = app.engine.store().get(&id, Some("operator")).await.unwrap().unwrap();
        assert_eq!(stored.content, "rejected revision");
        assert!(!stored.has_embedding);
        assert!(!app.engine.store().fetch_embeddings_for_ids(&[id]).await.unwrap().contains_key(&id));
    }

    #[tokio::test]
    async fn immediately_expired_save_closes_editor_as_committed() {
        let (mut app, mut rx) = app_with_memories(&["expires now"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let id = app.rows[0].memory.id;
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().expiry.value = "2000-01-01T00:00:00Z".into();

        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(app.mode, Mode::Browse);
        assert!(!app.pending);
        assert!(app.edit.is_none());
        assert!(app.detail.is_none());
        assert!(!app.rows.iter().any(|row| row.memory.id == id));
        assert!(matches!(&app.status, Status::Held(message) if message.contains("revised")));
    }

    #[tokio::test]
    async fn metadata_only_edit_does_not_initialize_embedding() {
        let store = SqliteStore::in_memory().unwrap();
        let memory = Memory::new_for_test("unchanged".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        let id = store.store(&memory, None).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0_usize));
        let engine = LocalHoldEngine::new(
            store,
            Arc::new(CountingEmbedding { calls: Arc::clone(&calls) }),
            LimitsConfig::default(),
            SearchConfig::default(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().metadata.value = r#"{"summary":"card only","agent_label":null}"#.into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(calls.load(Ordering::SeqCst), 0_usize);
        assert_eq!(app.engine.store().get(&id, Some("operator")).await.unwrap().unwrap().content, "unchanged");
        assert_eq!(app.engine.get_metadata(&id).await.unwrap().unwrap().summary.as_deref(), Some("card only"));
    }

    #[tokio::test]
    async fn delete_requires_confirmation_and_keeps_nearest_selection() {
        let (mut app, mut rx) = app_with_memories(&["first", "second"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let id = app.rows[0].memory.id;

        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('d'))).await;
        assert_eq!(app.mode, Mode::ConfirmDelete);
        app.on_event(press(KeyCode::Char('y'))).await;
        assert!(app.pending);
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(app.mode, Mode::Browse);
        assert!(!app.rows.iter().any(|row| row.memory.id == id));
        assert!(app.engine.store().get(&id, Some("operator")).await.unwrap().is_none());
        assert_eq!(app.row_selected, 0_usize);
    }

    #[tokio::test]
    async fn stale_edit_is_refused_and_draft_is_preserved() {
        let (mut app, mut rx) = app_with_memories(&["original"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let id = app.rows[0].memory.id;
        app.on_event(press(KeyCode::Enter)).await;

        let external = MemoryUpdate {
            content: Some("external revision".into()),
            ..MemoryUpdate::default()
        };
        let outcome = app.engine.update_memory(id, external, "operator").await.unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Applied);

        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().content.value = "stale local revision".into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(app.mode, Mode::Edit);
        assert!(app.edit.as_ref().unwrap().dirty());
        assert!(matches!(app.status, Status::NotHeld(_)));
        let stored = app.engine.store().get(&id, Some("operator")).await.unwrap().unwrap();
        assert_eq!(stored.content, "external revision");
    }

    #[tokio::test]
    async fn stale_content_is_not_sent_to_embedding_provider() {
        let store = SqliteStore::in_memory().unwrap();
        let memory = Memory::new_for_test("original".into(), Vec::new(), Provenance::default(), AccessPolicy::Public);
        let id = store.store(&memory, None).await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0_usize));
        let engine = LocalHoldEngine::new(
            store,
            Arc::new(CountingEmbedding { calls: Arc::clone(&calls) }),
            LimitsConfig::default(),
            SearchConfig::default(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;

        let external = MemoryUpdate {
            tags: Some(vec!["external".into()]),
            ..MemoryUpdate::default()
        };
        let outcome = app.engine.store().update_authorized(&id, &external, "operator").await.unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Applied);

        app.on_event(press(KeyCode::Char('e'))).await;
        app.edit.as_mut().unwrap().content.value = "sensitive stale draft".into();
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(calls.load(Ordering::SeqCst), 0_usize);
        assert_eq!(app.mode, Mode::Edit);
        assert!(app.edit.as_ref().unwrap().dirty());
    }

    #[test]
    fn scope_items_use_registry_names_unique_suffixes_and_nonzero_counts() {
        let definitions = vec![ScopeDefinition {
            scope_key: "org/registered".into(),
            display_name: "Registered Project".into(),
            description: None,
            aliases: Vec::new(),
            matchers: Vec::new(),
            parent: None,
            related: Vec::new(),
        }];
        let items = build_scope_items(definitions, vec![
            ("/worktrees/alpha/project".into(), 2_u64),
            ("/worktrees/beta/project".into(), 1_u64),
            ("org/registered".into(), 3_u64),
            ("unused".into(), 0_u64),
        ]);

        assert_eq!(items.iter().map(|item| (item.label.as_str(), item.count)).collect::<Vec<_>>(), vec![
            ("alpha/project", 2_u64),
            ("beta/project", 1_u64),
            ("Registered Project", 3_u64)
        ]);
    }

    #[tokio::test]
    async fn stale_facets_are_dropped_and_missing_selection_falls_back_to_all() {
        let (mut app, mut rx) = app_with_memories(&["visible"]).await;
        app.facet_generation = 2_u64;
        app.on_data(DataMsg::ScopeFacets {
            definitions: Vec::new(),
            by_scope: vec![("stale".into(), 9_u64)],
            total: 9_u64,
            registry_warning: None,
            generation: 1_u64,
        });
        assert!(app.scopes.is_empty());
        assert!(app.scope_total.is_none());

        app.scopes = build_scope_items(Vec::new(), vec![("gone".into(), 1_u64)]);
        app.scope_selected = 1_usize;
        app.on_data(DataMsg::ScopeFacets {
            definitions: Vec::new(),
            by_scope: vec![("replacement".into(), 1_u64)],
            total: 1_u64,
            registry_warning: None,
            generation: 2_u64,
        });

        assert_eq!(app.scope_selected, 0_usize);
        assert!(app.loading);
        assert!(matches!(rx.recv().await, Some(DataMsg::Rows { .. })));
    }

    #[tokio::test]
    async fn scope_sidebar_lists_unregistered_scopes_and_filters_live() {
        let store = SqliteStore::in_memory().unwrap();
        for (content, scope) in [("alpha memory", "/worktrees/alpha/project"), ("beta memory", "/worktrees/beta/project")] {
            let memory = Memory::new_for_test(content.into(), Vec::new(), Provenance::new_for_test(None, Some(scope.into()), None), AccessPolicy::Public);
            let _id = store.store(&memory, None).await.unwrap();
        }
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), None, tx);

        app.bootstrap().await;
        assert_eq!(app.scope_total, Some(2_u64));
        assert_eq!(app.scopes.iter().map(|scope| scope.label.as_str()).collect::<Vec<_>>(), vec![
            "alpha/project",
            "beta/project"
        ]);
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.rows.len(), 2_usize);

        app.on_event(press(KeyCode::Left)).await;
        assert_eq!(app.focus, Focus::Scopes);
        app.on_event(press(KeyCode::Down)).await;
        app.on_data(rx.recv().await.unwrap());

        assert_eq!(app.selected_scope().as_deref(), Some("/worktrees/alpha/project"));
        assert_eq!(app.rows.len(), 1_usize);
        assert_eq!(app.rows[0].memory.content, "alpha memory");

        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = rendered_text(buffer);
        assert!(rendered.contains("alpha/project"));
        assert!(rendered.contains("j/k filter"));
        assert_text_color(buffer, "alpha/project", app.theme.azure);
        app.on_event(press(KeyCode::End)).await;
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.selected_scope().as_deref(), Some("/worktrees/beta/project"));
        assert_eq!(app.rows[0].memory.content, "beta memory");

        app.on_event(press(KeyCode::BackTab)).await;
        assert_eq!(app.focus, Focus::Memories);
        app.on_event(press(KeyCode::Char('/'))).await;
        for ch in "beta".chars() {
            app.on_event(press(KeyCode::Char(ch))).await;
        }
        app.on_event(press(KeyCode::Enter)).await;
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.selected_scope().as_deref(), Some("/worktrees/beta/project"));
        assert_eq!(app.rows.len(), 1_usize);
        assert_eq!(app.rows[0].memory.content, "beta memory");
    }

    #[tokio::test]
    async fn frame_renders_brand_chrome() {
        let (mut app, mut rx) = app_with_memories(&["the keep stands"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.notice = Some("reranker off: artifacts are not cached".into());
        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = rendered_text(buffer);
        assert!(rendered.contains("localhold"), "header should carry the wordmark");
        assert!(
            rendered.contains("mode auto"),
            "the header should name the configured automatic mode before a search executes"
        );
        assert!(rendered.contains("SCOPES"), "scope pane should be titled");
        assert!(rendered.contains("MEMORIES"), "memory pane should be titled");
        assert!(rendered.contains('\u{2580}'), "the battlement rule should be drawn");
        assert!(rendered.contains("held"), "the status line should speak the brand verb");
        assert!(rendered.contains("reranker off"), "persistent startup notices should be visible in the TUI");
        assert_text_color(buffer, "semantic", Color::Reset);
        assert_gold_is_battlement_only(buffer, app.theme.or);
    }

    #[tokio::test]
    async fn header_tracks_requested_and_executed_search_modes() {
        let (mut app, mut rx) = app_with_memories(&["searchable memory"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();

        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode auto"));

        app.mode = Mode::Search;
        app.loading = true;
        app.requested_mode = Some(crate::types::SearchMode::Auto);
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode auto"));

        app.requested_mode = Some(crate::types::SearchMode::Keyword);
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode keyword"));

        app.on_data(DataMsg::Rows {
            rows: Vec::new(),
            mode: Some(crate::types::SearchMode::Text),
            generation: app.generation,
        });
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode text"));

        app.loading = true;
        app.requested_mode = Some(crate::types::SearchMode::Hybrid);
        app.on_data(DataMsg::Failed {
            message: "search failed".into(),
            generation: app.generation,
        });
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        assert!(rendered_text(terminal.backend().buffer()).contains("mode hybrid"));
        assert_eq!(app.executed_mode, None, "a failed refresh must not retain the previous executed mode");
    }

    #[tokio::test]
    async fn detail_uses_dim_border_and_keeps_gold_in_the_battlement() {
        let (mut app, mut rx) = app_with_memories(&["detail memory"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;

        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        assert!(rendered_text(buffer).contains("MEMORY"));
        assert_eq!(buffer[(0_u16, 1_u16)].fg, Color::Reset);
        assert!(buffer[(0_u16, 1_u16)].modifier.contains(Modifier::DIM));
        assert_gold_is_battlement_only(buffer, app.theme.or);
    }

    #[tokio::test]
    async fn edit_input_is_frozen_while_a_save_is_pending() {
        let (mut app, mut rx) = app_with_memories(&["original"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.pending = true;
        let original = app.edit.as_ref().unwrap().content.value.clone();

        app.on_event(press(KeyCode::Char('x'))).await;

        assert_eq!(app.edit.as_ref().unwrap().content.value, original);
    }

    #[tokio::test]
    async fn editor_and_confirmation_render_in_a_narrow_terminal() {
        let (mut app, mut rx) = app_with_memories(&["a narrow keep with a long memory"]).await;
        app.principal = Some("operator".into());
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.on_event(press(KeyCode::Enter)).await;
        app.on_event(press(KeyCode::Char('e'))).await;

        let mut terminal = Terminal::new(TestBackend::new(60_u16, 18_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let rendered: String = terminal.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(rendered.contains("EDIT MEMORY"));

        let long_line = "wrapped-content-".repeat(40_usize);
        let edit = app.edit.as_mut().unwrap();
        edit.content.value = long_line;
        edit.content.cursor = edit.content.value.len();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let rendered: String = terminal.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(
            rendered.contains(char::from_u32(0x2588_u32).unwrap()),
            "the edit cursor must remain visible after a long line wraps"
        );

        app.mode = Mode::ConfirmDelete;
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let rendered: String = terminal.backend().buffer().content().iter().map(ratatui::buffer::Cell::symbol).collect();
        assert!(rendered.contains("CONFIRM"));
        assert!(rendered.contains("Forget this memory"));
    }
}
