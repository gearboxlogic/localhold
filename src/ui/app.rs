//! Application state and update logic for `hold ui`.

use std::{collections::HashSet, fmt, future::Future, pin::Pin, sync::Arc};

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::Deserialize;
use tokio::sync::{OnceCell, mpsc::UnboundedSender};

use crate::{
    context::{
        ContextAnchorPolicy, ContextAnchorPolicyDraft, ContextAnchorPolicyRecord, ContextAuditDraft, ContextAuditEvent, ContextDefinitionPatch, ContextDescriptor, ContextGrant,
        ContextId, ContextIdentity, ContextIdentityInput, ContextKind, ContextKindDefinition, ContextKindDraft, ContextKindPolicy, ContextKindPolicyDraft, ContextKindPolicyRecord,
        ContextLifecycle, ContextPolicyLayer, ContextRecord, MAX_EFFECTIVE_CONTEXTS, OPERATOR_PRINCIPAL, evaluate_effective_context_policy, normalize_context_identity,
    },
    engine::{LocalHoldEngine, SearchRequest},
    store::MemoryStore,
    types::{AuditEntry, Memory, MemoryFilter, MemoryId, MemoryMetadata, QueryContext, SearchMode, WriteOutcome},
    ui::{
        editor::{EditDraft, ParsedEdit, TextInput},
        theme::Theme,
    },
};

/// Which pane owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// The context list on the left.
    Contexts,
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
    /// Governed context administration is open.
    ContextManager,
    /// One Context Manager pane is being edited as JSON.
    ContextManagerEdit,
    /// A dirty Context Manager edit is awaiting discard confirmation.
    ConfirmContextDiscard,
}

/// Context Manager panes. Definition facets remain separate so each edit has a
/// small, reviewable replacement contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextManagerPane {
    Kinds,
    Definition,
    Identities,
    Aliases,
    Hierarchy,
    Grants,
    Lifecycle,
    PrincipalPolicy,
    OperatorPolicy,
    AnchorOverride,
}

impl ContextManagerPane {
    pub(crate) const ALL: [Self; 10] = [
        Self::Kinds,
        Self::Definition,
        Self::Identities,
        Self::Aliases,
        Self::Hierarchy,
        Self::Grants,
        Self::Lifecycle,
        Self::PrincipalPolicy,
        Self::OperatorPolicy,
        Self::AnchorOverride,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Kinds => "KINDS",
            Self::Definition => "DEFINITION",
            Self::Identities => "IDENTITIES",
            Self::Aliases => "ALIASES",
            Self::Hierarchy => "HIERARCHY",
            Self::Grants => "GRANTS",
            Self::Lifecycle => "ARCHIVE / REACTIVATE",
            Self::PrincipalPolicy => "PRINCIPAL POLICY",
            Self::OperatorPolicy => "OPERATOR DEFAULTS",
            Self::AnchorOverride => "ANCHOR OVERRIDE",
        }
    }

    const fn next(self, backwards: bool) -> Self {
        let index: usize = match self {
            Self::Kinds => 0,
            Self::Definition => 1,
            Self::Identities => 2,
            Self::Aliases => 3,
            Self::Hierarchy => 4,
            Self::Grants => 5,
            Self::Lifecycle => 6,
            Self::PrincipalPolicy => 7,
            Self::OperatorPolicy => 8,
            Self::AnchorOverride => 9,
        };
        let next = if backwards {
            if index == 0 { Self::ALL.len().saturating_sub(1) } else { index.saturating_sub(1) }
        } else if index.saturating_add(1) == Self::ALL.len() {
            0
        } else {
            index.saturating_add(1)
        };
        Self::ALL[next]
    }
}

/// Loaded state for one governed context.
#[derive(Debug)]
pub(crate) struct ContextManager {
    pub record: ContextRecord,
    pub kinds: Vec<ContextKindDefinition>,
    pub grants: Vec<ContextGrant>,
    pub policies: Vec<ContextKindPolicyRecord>,
    pub anchor_policies: Vec<ContextAnchorPolicyRecord>,
    pub audit: Vec<ContextAuditEvent>,
    pub pane: ContextManagerPane,
    pub scroll: u16,
    pub edit: Option<ContextManagerEdit>,
}

/// A JSON replacement draft for the active manager pane.
#[derive(Debug)]
pub(crate) struct ContextManagerEdit {
    input: TextInput,
    original: String,
}

impl ContextManagerEdit {
    fn new(value: String) -> Self {
        Self {
            input: TextInput::new(value.clone()),
            original: value,
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        self.input.value != self.original
    }

    pub(crate) const fn input(&self) -> &TextInput {
        &self.input
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextKindEdit {
    kind: ContextKind,
    display_name: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextDefinitionEdit {
    display_name: String,
    description: Option<String>,
    guidance: Option<String>,
    resolver_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextIdentityEdit {
    retain_fingerprints: Vec<String>,
    #[serde(default)]
    add: Vec<ContextIdentityInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextHierarchyEdit {
    parent_id: Option<ContextId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextLifecycleEdit {
    lifecycle: ContextLifecycle,
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

/// One authorized governed context in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextItem {
    /// Definition and safe resolver metadata.
    pub record: ContextRecord,
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
    /// Ordered direct governed memberships.
    pub contexts: Vec<ContextDescriptor>,
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
    /// Authorized governed context catalog.
    ContextCatalog {
        /// Contexts visible to the TUI principal, including archived lineage.
        records: Vec<ContextRecord>,
        /// Total number of governed visible memories in broad-search mode.
        total: u64,
        /// Nonfatal catalog-loading warning.
        warning: Option<String>,
        /// Catalog generation; stale responses are dropped.
        generation: u64,
    },
    /// Context catalog loading failed while row browsing remains available.
    ContextCatalogFailed {
        /// Human-readable failure.
        message: String,
        /// Catalog generation; stale responses are dropped.
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
        /// Refreshed direct governed memberships.
        contexts: Vec<ContextDescriptor>,
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
#[expect(clippy::struct_excessive_bools, reason = "independent TUI state flags represent selection, mutation, loading, and quit state")]
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
    /// Stamp for in-flight context catalog tasks.
    pub context_generation: u64,
    /// Resolved tincture palette.
    pub theme: Theme,
    /// Clock snapshot used for relative ages, refreshed with the data.
    pub now: DateTime<Utc>,
    /// Authorized contexts currently shown in the sidebar.
    pub contexts: Vec<ContextItem>,
    /// Complete authorized catalog, including archived lineage nodes hidden
    /// from the default sidebar.
    pub context_records: Vec<ContextRecord>,
    /// Whether the context catalog includes archived records for management.
    pub show_archived_contexts: bool,
    /// Total number of governed memories represented by broad search.
    pub context_total: Option<u64>,
    /// Nonfatal context-loading notice.
    pub context_notice: Option<String>,
    /// Cursor index into the context pane (0 = broad authorized search).
    pub context_cursor: usize,
    /// Direct selected contexts in primary order.
    pub selected_context_ids: Vec<ContextId>,
    /// Whether selected parents include descendants.
    pub include_descendants: bool,
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
    /// Context Manager state, while administering a hovered context.
    pub context_manager: Option<ContextManager>,
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
            context_generation: 0_u64,
            theme,
            now,
            contexts: Vec::new(),
            context_records: Vec::new(),
            show_archived_contexts: false,
            context_total: None,
            context_notice: None,
            context_cursor: 0_usize,
            selected_context_ids: Vec::new(),
            include_descendants: false,
            rows: Vec::new(),
            row_selected: 0_usize,
            focus: Focus::Memories,
            mode: Mode::Browse,
            query: String::new(),
            requested_mode: None,
            executed_mode: None,
            detail: None,
            edit: None,
            context_manager: None,
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

    /// Kick off the first authorized context catalog and bounded listing.
    pub(crate) async fn bootstrap(&mut self) {
        let principal = self.principal.as_deref().unwrap_or("anonymous");
        let broad_filter = MemoryFilter {
            context_ids: Some(Vec::new()),
            ..MemoryFilter::default()
        };
        let (records, stats) = tokio::join!(
            load_context_records(&self.engine, principal, true),
            self.engine.count_memories(broad_filter, self.ctx(), 0_usize),
        );
        match stats {
            Ok(stats) => {
                let (records, warning) = match records {
                    Ok(records) => (records, None),
                    Err(error) => (Vec::new(), Some(format!("context catalog unavailable: {error}"))),
                };
                self.apply_context_catalog(records, Some(stats.total), warning);
            }
            Err(error) => {
                let warning = format!("governed memory count unavailable: {error}");
                match records {
                    Ok(records) => self.apply_context_catalog(records, None, Some(warning)),
                    Err(catalog_error) => {
                        self.context_notice = Some(format!("{warning}; context catalog unavailable: {catalog_error}"));
                    }
                }
            }
        }
        self.refresh();
    }

    fn ctx(&self) -> QueryContext {
        QueryContext {
            principal: self.principal.clone(),
        }
    }

    /// The context currently under the sidebar cursor.
    pub(crate) fn cursor_context(&self) -> Option<&ContextItem> {
        self.context_cursor.checked_sub(1_usize).and_then(|index| self.contexts.get(index))
    }

    /// Expand direct selections through ancestors and optional descendants.
    pub(crate) fn effective_selected_context_ids(&self) -> Result<Vec<ContextId>, String> {
        if self.selected_context_ids.len() > MAX_EFFECTIVE_CONTEXTS {
            return Err(effective_context_limit_message());
        }
        let mut effective = self.selected_context_ids.clone();
        let mut included = effective.iter().copied().collect::<HashSet<_>>();
        for selected in &self.selected_context_ids {
            let cursor = self.context_parent_id(*selected);
            self.append_context_ancestors(cursor, &mut included, &mut effective)?;
        }
        if self.include_descendants {
            self.append_context_descendants(&mut included, &mut effective)?;
        }
        Ok(effective)
    }

    fn context_parent_id(&self, context_id: ContextId) -> Option<ContextId> {
        self.context_records
            .iter()
            .find(|record| record.context.id == context_id)
            .and_then(|record| record.context.parent_id)
    }

    fn context_is_active(&self, context_id: ContextId) -> bool {
        self.context_records
            .iter()
            .find(|record| record.context.id == context_id)
            .is_some_and(|record| record.context.lifecycle == ContextLifecycle::Active)
    }

    fn append_context_ancestors(&self, mut cursor: Option<ContextId>, included: &mut HashSet<ContextId>, effective: &mut Vec<ContextId>) -> Result<(), String> {
        let mut visited = HashSet::new();
        while let Some(parent_id) = cursor {
            if !visited.insert(parent_id) {
                break;
            }
            if self.context_is_active(parent_id) && !included.contains(&parent_id) {
                append_effective_context(parent_id, included, effective)?;
            }
            cursor = self.context_parent_id(parent_id);
        }
        Ok(())
    }

    fn has_selected_context_ancestor(&self, mut cursor: Option<ContextId>) -> bool {
        let mut visited = HashSet::new();
        while let Some(parent_id) = cursor {
            if !visited.insert(parent_id) {
                return false;
            }
            if self.selected_context_ids.contains(&parent_id) {
                return true;
            }
            cursor = self.context_parent_id(parent_id);
        }
        false
    }

    fn append_context_descendants(&self, included: &mut HashSet<ContextId>, effective: &mut Vec<ContextId>) -> Result<(), String> {
        for record in &self.context_records {
            let context_id = record.context.id;
            if record.context.lifecycle == ContextLifecycle::Active && !included.contains(&context_id) && self.has_selected_context_ancestor(record.context.parent_id) {
                append_effective_context(context_id, included, effective)?;
            }
        }
        Ok(())
    }

    fn discard_context_manager_edit(&mut self) {
        if let Some(manager) = self.context_manager.as_mut() {
            manager.edit = None;
        }
    }

    fn filter(&self, limit: Option<usize>) -> Result<MemoryFilter, String> {
        Ok(MemoryFilter {
            context_ids: Some(self.effective_selected_context_ids()?),
            limit,
            ..Default::default()
        })
    }

    /// Re-run the visible listing or search under the current filter.
    pub(crate) fn refresh(&mut self) {
        self.generation = self.generation.saturating_add(1_u64);
        self.loading = true;
        self.now = self.engine.now();
        if self.query.is_empty() { self.spawn_list() } else { self.spawn_search() }
    }

    fn refresh_all(&mut self) {
        self.refresh_context_catalog();
        self.refresh();
    }

    fn refresh_context_catalog(&mut self) {
        self.context_generation = self.context_generation.saturating_add(1_u64);
        let generation = self.context_generation;
        let engine = self.engine.clone();
        let tx = self.data_tx.clone();
        let ctx = self.ctx();
        let principal = self.principal.clone().unwrap_or_else(|| "anonymous".into());
        #[expect(unused_results, reason = "JoinHandle intentionally dropped — the result arrives via the data channel")]
        tokio::spawn(async move {
            let broad_filter = MemoryFilter {
                context_ids: Some(Vec::new()),
                ..MemoryFilter::default()
            };
            let (records, stats) = tokio::join!(load_context_records(&engine, &principal, true), engine.count_memories(broad_filter, ctx, 0_usize),);
            let msg = match stats {
                Ok(stats) => {
                    let (records, warning) = match records {
                        Ok(records) => (records, None),
                        Err(error) => (Vec::new(), Some(format!("context catalog unavailable: {error}"))),
                    };
                    DataMsg::ContextCatalog {
                        records,
                        total: stats.total,
                        warning,
                        generation,
                    }
                }
                Err(error) => DataMsg::ContextCatalogFailed {
                    message: format!("governed memory count unavailable: {error}"),
                    generation,
                },
            };
            drop(tx.send(msg));
        });
    }

    fn apply_context_catalog(&mut self, records: Vec<ContextRecord>, total: Option<u64>, warning: Option<String>) {
        let cursor_id = self.cursor_context().map(|item| item.record.context.id);
        self.context_records = records;
        self.contexts = self
            .context_records
            .iter()
            .filter(|record| self.show_archived_contexts || record.context.lifecycle == ContextLifecycle::Active)
            .cloned()
            .map(|record| ContextItem { record })
            .collect();
        self.context_total = total;
        self.context_notice = warning;
        let available = self
            .contexts
            .iter()
            .filter(|item| item.record.context.lifecycle == ContextLifecycle::Active)
            .map(|item| item.record.context.id)
            .collect::<HashSet<_>>();
        self.selected_context_ids.retain(|id| available.contains(id));
        self.context_cursor = cursor_id
            .and_then(|id| self.contexts.iter().position(|item| item.record.context.id == id))
            .map_or(0, |index| index.saturating_add(1));
    }

    fn spawn_list(&self) {
        let engine = self.engine.clone();
        let tx = self.data_tx.clone();
        let generation = self.generation;
        let filter = match self.filter(Some(200_usize)) {
            Ok(filter) => filter,
            Err(message) => {
                drop(tx.send(DataMsg::Failed { message, generation }));
                return;
            }
        };
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
        let filter = match self.filter(None) {
            Ok(filter) => filter,
            Err(message) => {
                drop(tx.send(DataMsg::Failed { message, generation }));
                return;
            }
        };
        let request = SearchRequest {
            query: self.query.clone(),
            limit: 50_usize,
            filter,
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

    fn apply_rows(&mut self, rows: Vec<Row>, mode: Option<SearchMode>) {
        self.rows = rows.into_iter().map(sanitize_row_for_view).collect();
        self.executed_mode = mode;
        self.loading = false;
        self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
        self.status = self.results_status();
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a completed memory refresh supplies the memory, metadata, audit, contexts, and any warning as one state transition"
    )]
    fn apply_updated_memory(
        &mut self,
        memory: Memory,
        metadata: Option<MemoryMetadata>,
        mut audit: Vec<AuditEntry>,
        contexts: Vec<ContextDescriptor>,
        refresh_warning: Option<String>,
    ) {
        self.pending = false;
        let refresh_results = !self.query.is_empty();
        let memory = memory.sanitize_for_wire();
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
            contexts,
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

    fn remove_memory_from_view(&mut self, id: MemoryId, status: Status, refresh_context_catalog: bool) {
        self.pending = false;
        self.rows.retain(|row| row.memory.id != id);
        self.row_selected = self.row_selected.min(self.rows.len().saturating_sub(1_usize));
        self.detail = None;
        self.edit = None;
        self.mode = Mode::Browse;
        self.status = status;
        if refresh_context_catalog {
            self.refresh_context_catalog();
        }
    }

    /// Fold a completed data or mutation task into the state.
    pub(crate) fn on_data(&mut self, msg: DataMsg) {
        match msg {
            DataMsg::ContextCatalog {
                records,
                total,
                warning,
                generation,
            } if generation == self.context_generation => {
                self.apply_context_catalog(records, Some(total), warning);
            }
            DataMsg::ContextCatalogFailed { message, generation } if generation == self.context_generation => {
                self.context_notice = Some(message);
            }
            DataMsg::Rows { rows, mode, generation } if generation == self.generation => {
                self.apply_rows(rows, mode);
            }
            DataMsg::Failed { message, generation } if generation == self.generation => {
                self.executed_mode = None;
                self.loading = false;
                self.status = Status::NotHeld(message);
            }
            DataMsg::Updated {
                memory,
                metadata,
                audit,
                contexts,
                refresh_warning,
                generation,
            } if generation == self.operation_generation => {
                self.apply_updated_memory(*memory, metadata, audit, contexts, refresh_warning);
            }
            DataMsg::UpdatedInvisible { id, generation } if generation == self.operation_generation => {
                self.remove_memory_from_view(id, Status::Held("memory revised and is no longer visible".into()), true);
            }
            DataMsg::UpdatedUnrefreshed { id, message, generation } if generation == self.operation_generation => {
                self.remove_memory_from_view(id, Status::NotHeld(format!("memory revised, but refresh failed: {message}")), false);
            }
            DataMsg::Missing { id, generation } if generation == self.operation_generation => {
                self.remove_memory_from_view(id, Status::NotHeld("memory no longer exists".into()), true);
            }
            DataMsg::Deleted { id, generation } if generation == self.operation_generation => {
                self.remove_memory_from_view(id, Status::Held("memory forgotten".into()), true);
            }
            DataMsg::MutationFailed { message, generation } if generation == self.operation_generation => {
                self.pending = false;
                self.status = Status::NotHeld(message);
                if self.mode == Mode::ConfirmDelete {
                    self.mode = Mode::Detail;
                }
            }
            DataMsg::ContextCatalog { .. }
            | DataMsg::ContextCatalogFailed { .. }
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
        let context = if self.selected_context_ids.is_empty() {
            " \u{b7} broad authorized search".to_owned()
        } else {
            format!(" \u{b7} {} direct contexts", self.selected_context_ids.len())
        };
        if self.query.is_empty() {
            return browse_results_status(count, &context);
        }
        let mode = self.executed_mode.map_or_else(String::new, |mode| format!(" ({mode})"));
        if count == 0_usize {
            return Status::Note(format!("nothing found{mode}{context}"));
        }
        Status::Held(format!("{count} results{mode}{context}"))
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
                Mode::ContextManager => self.key_context_manager(key).await,
                Mode::ContextManagerEdit => self.key_context_manager_edit(key).await,
                Mode::ConfirmContextDiscard => self.key_confirm_context_discard(key),
            }
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    async fn key_browse(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab | KeyCode::BackTab => self.focus = if self.focus == Focus::Contexts { Focus::Memories } else { Focus::Contexts },
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Contexts,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Memories,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(true),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(false),
            KeyCode::Char('g') | KeyCode::Home => self.jump_selection(true),
            KeyCode::Char('G') | KeyCode::End => self.jump_selection(false),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('m') => self.cycle_mode(),
            KeyCode::Char('r') => self.refresh_all(),
            KeyCode::Char(' ') if self.focus == Focus::Contexts => self.toggle_cursor_context().await,
            KeyCode::Char('x') if self.focus == Focus::Contexts => {
                self.selected_context_ids.clear();
                self.row_selected = 0;
                self.refresh();
            }
            KeyCode::Char('D') if self.focus == Focus::Contexts => {
                self.include_descendants = !self.include_descendants;
                self.row_selected = 0;
                self.refresh();
            }
            KeyCode::Char('a') if self.focus == Focus::Contexts => {
                self.show_archived_contexts = !self.show_archived_contexts;
                self.refresh_context_catalog();
            }
            KeyCode::Char('c') if self.focus == Focus::Contexts => self.open_context_manager().await,
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

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    async fn key_context_manager(&mut self, key: KeyEvent) {
        let Some(manager) = self.context_manager.as_mut() else {
            self.mode = Mode::Browse;
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.context_manager = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                manager.pane = manager.pane.next(false);
                manager.scroll = 0;
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                manager.pane = manager.pane.next(true);
                manager.scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => manager.scroll = manager.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => manager.scroll = manager.scroll.saturating_sub(1),
            KeyCode::Char('e') => self.begin_context_manager_edit(),
            KeyCode::Char('r') => self.reload_context_manager().await,
            _other => {}
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    async fn key_context_manager_edit(&mut self, key: KeyEvent) {
        if self.pending {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.save_context_manager_edit().await;
            return;
        }
        if key.code == KeyCode::Esc {
            let dirty = self
                .context_manager
                .as_ref()
                .and_then(|manager| manager.edit.as_ref())
                .is_some_and(ContextManagerEdit::dirty);
            if dirty {
                self.mode = Mode::ConfirmContextDiscard;
            } else {
                self.discard_context_manager_edit();
                self.mode = Mode::ContextManager;
            }
            return;
        }
        let Some(edit) = self.context_manager.as_mut().and_then(|manager| manager.edit.as_mut()) else {
            self.mode = Mode::ContextManager;
            return;
        };
        match key.code {
            KeyCode::Enter => edit.input.insert('\n'),
            KeyCode::Backspace => edit.input.backspace(),
            KeyCode::Delete => edit.input.delete(),
            KeyCode::Left => edit.input.left(),
            KeyCode::Right => edit.input.right(),
            KeyCode::Home => edit.input.home(),
            KeyCode::End => edit.input.end(),
            KeyCode::Up => edit.input.up(),
            KeyCode::Down => edit.input.down(),
            KeyCode::Char(ch) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => edit.input.insert(ch),
            _other => {}
        }
    }

    #[expect(clippy::wildcard_enum_match_arm, reason = "KeyCode is non-exhaustive upstream; unmapped keys are intentionally ignored")]
    fn key_confirm_context_discard(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') => {
                if let Some(manager) = self.context_manager.as_mut() {
                    manager.edit = None;
                }
                self.mode = Mode::ContextManager;
            }
            KeyCode::Char('n') | KeyCode::Esc => self.mode = Mode::ContextManagerEdit,
            _other => {}
        }
    }

    async fn open_context_manager(&mut self) {
        let Some(item) = self.cursor_context() else {
            self.status = Status::Note("choose a context before opening its manager".into());
            return;
        };
        let id = item.record.context.id;
        let principal = self.principal.clone().unwrap_or_else(|| "anonymous".into());
        match load_context_manager(self.engine.store(), id, &principal, ContextManagerPane::Definition).await {
            Ok(manager) => {
                self.context_manager = Some(manager);
                self.mode = Mode::ContextManager;
            }
            Err(error) => self.status = Status::NotHeld(format!("context manager unavailable: {error}")),
        }
    }

    fn begin_context_manager_edit(&mut self) {
        let Some(principal) = self.principal.as_deref() else {
            self.status = Status::NotHeld("context editing requires a configured principal".into());
            return;
        };
        if self
            .context_manager
            .as_ref()
            .is_some_and(|manager| matches!(manager.pane, ContextManagerPane::Kinds | ContextManagerPane::OperatorPolicy))
            && principal != OPERATOR_PRINCIPAL
        {
            self.status = Status::NotHeld(format!("operator context controls require --principal {OPERATOR_PRINCIPAL}"));
            return;
        }
        let Some(manager) = self.context_manager.as_mut() else { return };
        let value = context_manager_edit_value(manager);
        manager.edit = Some(ContextManagerEdit::new(value));
        self.mode = Mode::ContextManagerEdit;
    }

    async fn reload_context_manager(&mut self) {
        let Some(manager) = self.context_manager.as_ref() else { return };
        let id = manager.record.context.id;
        let pane = manager.pane;
        let principal = self.principal.clone().unwrap_or_else(|| "anonymous".into());
        match load_context_manager(self.engine.store(), id, &principal, pane).await {
            Ok(manager) => {
                self.context_manager = Some(manager);
                self.status = Status::Held("context manager refreshed".into());
            }
            Err(error) => self.status = Status::NotHeld(format!("context manager refresh failed: {error}")),
        }
    }

    async fn save_context_manager_edit(&mut self) {
        let Some(principal) = self.principal.clone() else {
            self.status = Status::NotHeld("context editing requires a configured principal".into());
            self.mode = Mode::ContextManager;
            return;
        };
        let Some(manager) = self.context_manager.as_ref() else { return };
        let Some(edit) = manager.edit.as_ref() else { return };
        let context_id = manager.record.context.id;
        let pane = manager.pane;
        let value = edit.input.value.clone();
        self.pending = true;
        let result = match self.mutation_engine.get().await {
            Ok(engine) => apply_context_manager_edit(engine.store(), manager, pane, &value, &principal).await,
            Err(error) => Err(error),
        };
        self.pending = false;
        match result {
            Ok(()) => {
                let previous_pane = pane;
                match load_context_manager(self.engine.store(), context_id, &principal, previous_pane).await {
                    Ok(manager) => {
                        let remains_selectable = manager.record.context.lifecycle == ContextLifecycle::Active;
                        self.selected_context_ids.retain(|selected| remains_selectable || *selected != context_id);
                        self.context_manager = Some(manager);
                        self.mode = Mode::ContextManager;
                        self.status = Status::Held(format!("{} updated", previous_pane.label().to_ascii_lowercase()));
                        self.refresh_all();
                    }
                    Err(error) => {
                        self.context_manager = None;
                        self.mode = Mode::Browse;
                        self.status = Status::NotHeld(format!("context updated, but refresh failed: {error}"));
                        self.refresh_all();
                    }
                }
            }
            Err(error) => {
                self.status = Status::NotHeld(error);
            }
        }
    }

    async fn open_or_focus(&mut self) {
        match self.focus {
            Focus::Contexts => self.focus = Focus::Memories,
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
                let contexts = if memory.was_redacted { Vec::new() } else { self.load_detail_contexts(&id).await };
                self.detail = Some(Detail {
                    memory,
                    metadata,
                    audit,
                    contexts,
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

    async fn load_detail_contexts(&mut self, id: &MemoryId) -> Vec<ContextDescriptor> {
        let principal = self.principal.as_deref().unwrap_or("anonymous");
        match self.engine.store().get_memory_contexts(id, principal).await {
            Ok(contexts) => contexts.iter().map(|membership| ContextDescriptor::from(&membership.context)).collect(),
            Err(error) => {
                self.status = Status::NotHeld(format!("contexts unavailable: {error}"));
                Vec::new()
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
        self.edit = Some(EditDraft::new(&detail.memory, detail.metadata.as_ref(), &detail.contexts));
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
        let ParsedEdit {
            update,
            metadata_patch,
            context_ids,
        } = match edit.parse() {
            Ok(parsed) => parsed,
            Err(error) => {
                edit.field = error.field;
                self.status = Status::NotHeld(error.message);
                return;
            }
        };
        let parsed = ParsedEdit {
            update,
            metadata_patch,
            context_ids,
        };
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
                    .update_memory_if_unmodified_with_metadata_contexts(id, expected_revision, parsed.update, parsed.metadata_patch, parsed.context_ids, &principal)
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

    async fn toggle_cursor_context(&mut self) {
        let Some(context_id) = self.cursor_context().map(|item| item.record.context.id) else {
            self.selected_context_ids.clear();
            self.row_selected = 0;
            self.refresh();
            return;
        };
        if self.cursor_context().is_some_and(|item| item.record.context.lifecycle == ContextLifecycle::Archived) {
            self.status = Status::Note("archived contexts can be managed or reactivated, but not selected".into());
            return;
        }
        if let Some(index) = self.selected_context_ids.iter().position(|selected| *selected == context_id) {
            let _removed = self.selected_context_ids.remove(index);
        } else {
            let mut proposed = self.selected_context_ids.clone();
            proposed.push(context_id);
            match self.context_selection_denial(&proposed).await {
                Ok(Some(message)) => {
                    self.status = Status::NotHeld(message);
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.status = Status::NotHeld(format!("context policy evaluation failed: {error}"));
                    return;
                }
            }
            self.selected_context_ids.push(context_id);
        }
        self.row_selected = 0;
        self.refresh();
    }

    async fn context_selection_denial(&self, direct_ids: &[ContextId]) -> Result<Option<String>, crate::error::EngineError> {
        let principal = self.principal.as_deref().unwrap_or("anonymous");
        let store = self.engine.store();
        let (effective, kinds, kind_policies, anchor_policies) = tokio::try_join!(
            store.expand_context_selection(direct_ids, principal, false),
            store.list_context_kinds(),
            store.list_context_kind_policies(principal),
            store.list_context_anchor_policies(principal),
        )?;
        let effective_ids = effective.iter().map(|context| context.id).collect::<Vec<_>>();
        let direct_kinds = direct_ids
            .iter()
            .filter_map(|id| self.context_records.iter().find(|record| record.context.id == *id))
            .map(|record| record.context.kind.clone())
            .collect::<HashSet<_>>();
        let policies = kinds
            .iter()
            .map(|definition| evaluate_effective_context_policy(&definition.kind, &kinds, &kind_policies, &anchor_policies, &self.context_records, &effective_ids))
            .collect::<Vec<_>>();
        if let Some(policy) = policies.iter().find(|policy| !policy.ambiguities.is_empty()) {
            return Ok(Some(format!("effective policy for {} is ambiguous", policy.kind)));
        }
        if let Some(policy) = policies.iter().find(|policy| direct_kinds.contains(&policy.kind) && !policy.allowed) {
            return Ok(Some(format!("effective policy denies context kind {}", policy.kind)));
        }
        for policy in policies.iter().filter(|policy| direct_kinds.contains(&policy.kind)) {
            if let Some(allowed_companions) = &policy.allowed_companion_kinds
                && let Some(disallowed) = direct_kinds
                    .iter()
                    .find(|candidate_kind| *candidate_kind != &policy.kind && !allowed_companions.contains(candidate_kind))
            {
                return Ok(Some(format!("effective {} policy does not allow companion kind {disallowed}", policy.kind)));
            }
        }
        Ok(None)
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
            Focus::Contexts => {
                let last = self.contexts.len();
                let next = if down {
                    self.context_cursor.saturating_add(1_usize).min(last)
                } else {
                    self.context_cursor.saturating_sub(1_usize)
                };
                self.context_cursor = next;
            }
        }
    }

    const fn jump_selection(&mut self, top: bool) {
        match self.focus {
            Focus::Memories => {
                self.row_selected = if top { 0_usize } else { self.rows.len().saturating_sub(1_usize) };
            }
            Focus::Contexts => self.context_cursor = if top { 0_usize } else { self.contexts.len() },
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

async fn load_context_records<S>(engine: &LocalHoldEngine<S>, principal: &str, include_archived: bool) -> Result<Vec<ContextRecord>, crate::error::EngineError>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut records = Vec::new();
    loop {
        let page = engine.store().list_context_records(principal, include_archived, records.len(), 500).await?;
        let page_len = page.len();
        records.extend(page);
        if page_len < 500 {
            break;
        }
    }
    Ok(records)
}

async fn load_context_manager<S>(store: &S, context_id: ContextId, principal: &str, pane: ContextManagerPane) -> Result<ContextManager, String>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let mut records = Vec::new();
    loop {
        let batch = store.list_context_records(principal, true, records.len(), 500).await.map_err(|error| error.to_string())?;
        let page_len = batch.len();
        records.extend(batch);
        if page_len < 500 {
            break;
        }
    }
    let record = records
        .into_iter()
        .find(|record| record.context.id == context_id)
        .ok_or_else(|| format!("context {context_id} is no longer visible"))?;
    let (kinds, grants, policies, anchor_policies, audit) = tokio::try_join!(
        store.list_context_kinds(),
        store.list_context_grants(&context_id, principal),
        store.list_context_kind_policies(principal),
        store.list_context_anchor_policies(principal),
        store.query_context_audit(&context_id, principal, 50),
    )
    .map_err(|error| error.to_string())?;
    Ok(ContextManager {
        record,
        kinds,
        grants,
        policies,
        anchor_policies,
        audit,
        pane,
        scroll: 0,
        edit: None,
    })
}

pub(crate) fn context_manager_edit_value(manager: &ContextManager) -> String {
    let context = &manager.record.context;
    let value = match manager.pane {
        ContextManagerPane::Kinds => {
            let definition = manager.kinds.iter().find(|definition| definition.kind == context.kind);
            serde_json::json!({
                "kind": context.kind,
                "display_name": definition.map_or(context.kind.as_str(), |definition| definition.display_name.as_str()),
                "enabled": definition.is_none_or(|definition| definition.enabled),
            })
        }
        ContextManagerPane::Definition => serde_json::json!({
            "display_name": context.display_name,
            "description": context.description,
            "guidance": context.guidance,
            "resolver_hints": manager.record.hints,
        }),
        ContextManagerPane::Identities => serde_json::json!({
            "retain_fingerprints": manager.record.identities.iter().map(|identity| identity.fingerprint.as_str()).collect::<Vec<_>>(),
            "add": Vec::<ContextIdentityInput>::new(),
        }),
        ContextManagerPane::Aliases => serde_json::json!(manager.record.aliases),
        ContextManagerPane::Hierarchy => serde_json::json!({ "parent_id": context.parent_id }),
        ContextManagerPane::Grants => {
            serde_json::json!(manager.grants.iter().map(|grant| grant.grantee_principal.as_str()).collect::<Vec<_>>())
        }
        ContextManagerPane::Lifecycle => serde_json::json!({ "lifecycle": context.lifecycle }),
        ContextManagerPane::PrincipalPolicy => serde_json::to_value(
            manager
                .policies
                .iter()
                .find(|record| record.layer == ContextPolicyLayer::Principal && record.kind == context.kind)
                .map_or_else(ContextKindPolicy::default, |record| record.policy.clone()),
        )
        .unwrap_or_else(|_| serde_json::json!({})),
        ContextManagerPane::OperatorPolicy => serde_json::to_value(
            manager
                .policies
                .iter()
                .find(|record| record.layer == ContextPolicyLayer::Operator && record.kind == context.kind)
                .map_or_else(ContextKindPolicy::default, |record| record.policy.clone()),
        )
        .unwrap_or_else(|_| serde_json::json!({})),
        ContextManagerPane::AnchorOverride => serde_json::to_value(
            manager
                .anchor_policies
                .iter()
                .find(|record| record.anchor_context_id == context.id)
                .map_or_else(ContextAnchorPolicy::default, |record| record.policy.clone()),
        )
        .unwrap_or_else(|_| serde_json::json!({ "kinds": {} })),
    };
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

fn context_definition_patch(
    manager: &ContextManager,
    definition: Option<ContextDefinitionEdit>,
    aliases: Option<Vec<String>>,
    identities: Option<Vec<ContextIdentity>>,
) -> ContextDefinitionPatch {
    let context = &manager.record.context;
    let definition = definition.unwrap_or_else(|| ContextDefinitionEdit {
        display_name: context.display_name.clone(),
        description: context.description.clone(),
        guidance: context.guidance.clone(),
        resolver_hints: manager.record.hints.clone(),
    });
    ContextDefinitionPatch {
        display_name: definition.display_name,
        description: definition.description,
        guidance: definition.guidance,
        aliases: aliases.unwrap_or_else(|| manager.record.aliases.clone()),
        identities: identities.unwrap_or_else(|| manager.record.identities.clone()),
        resolver_hints: definition.resolver_hints,
    }
}

fn context_manager_audit(principal: &str, context_id: ContextId, action: &str) -> ContextAuditDraft {
    ContextAuditDraft {
        actor_principal: principal.into(),
        action: action.into(),
        context_id: Some(context_id),
        memory_id: None,
        details: None,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the Context Manager dispatches ten typed JSON panes through their corresponding atomic store operations"
)]
async fn apply_context_manager_edit<S>(store: &S, manager: &ContextManager, pane: ContextManagerPane, value: &str, principal: &str) -> Result<(), String>
where
    S: MemoryStore + Clone + fmt::Debug + 'static,
{
    let context = &manager.record.context;
    let parse_error = |error: serde_json::Error| format!("invalid {} JSON: {error}", pane.label().to_ascii_lowercase());
    match pane {
        ContextManagerPane::Kinds => {
            let edit: ContextKindEdit = serde_json::from_str(value).map_err(parse_error)?;
            store
                .upsert_context_kind(
                    &ContextKindDraft {
                        kind: edit.kind,
                        display_name: edit.display_name,
                        enabled: edit.enabled,
                    },
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_kind_upserted"),
                )
                .await
        }
        ContextManagerPane::Definition => {
            let edit: ContextDefinitionEdit = serde_json::from_str(value).map_err(parse_error)?;
            store
                .update_context_definition(
                    &context.id,
                    &context_definition_patch(manager, Some(edit), None, None),
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_definition_updated"),
                )
                .await
        }
        ContextManagerPane::Identities => {
            let edit: ContextIdentityEdit = serde_json::from_str(value).map_err(parse_error)?;
            let requested = edit.retain_fingerprints.into_iter().collect::<HashSet<_>>();
            if requested.len() > manager.record.identities.len() {
                return Err("retained identity fingerprints must be unique".into());
            }
            let mut identities = manager
                .record
                .identities
                .iter()
                .filter(|identity| requested.contains(&identity.fingerprint))
                .cloned()
                .collect::<Vec<_>>();
            if identities.len() != requested.len() {
                return Err("every retained identity fingerprint must reference an existing canonical identity".into());
            }
            for input in edit.add {
                identities.push(normalize_context_identity(&input).map_err(|error| format!("invalid identity: {error}"))?);
            }
            let mut seen = HashSet::new();
            if identities
                .iter()
                .any(|identity| !seen.insert((identity.scheme.clone(), identity.namespace.clone(), identity.fingerprint.clone())))
            {
                return Err("identity entries must be unique".into());
            }
            store
                .update_context_definition(
                    &context.id,
                    &context_definition_patch(manager, None, None, Some(identities)),
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_identities_updated"),
                )
                .await
        }
        ContextManagerPane::Aliases => {
            let aliases: Vec<String> = serde_json::from_str(value).map_err(parse_error)?;
            store
                .update_context_definition(
                    &context.id,
                    &context_definition_patch(manager, None, Some(aliases), None),
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_aliases_updated"),
                )
                .await
        }
        ContextManagerPane::Hierarchy => {
            let edit: ContextHierarchyEdit = serde_json::from_str(value).map_err(parse_error)?;
            store
                .set_context_parent(
                    &context.id,
                    edit.parent_id.as_ref(),
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_parent_updated"),
                )
                .await
        }
        ContextManagerPane::Grants => {
            let grantees: Vec<String> = serde_json::from_str(value).map_err(parse_error)?;
            store
                .replace_context_grants(
                    &context.id,
                    &grantees,
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_grants_replaced"),
                )
                .await
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
        ContextManagerPane::Lifecycle => {
            let edit: ContextLifecycleEdit = serde_json::from_str(value).map_err(parse_error)?;
            store
                .set_context_lifecycle(
                    &context.id,
                    edit.lifecycle,
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_lifecycle_updated"),
                )
                .await
        }
        ContextManagerPane::PrincipalPolicy => {
            let policy: ContextKindPolicy = serde_json::from_str(value).map_err(parse_error)?;
            store
                .upsert_context_kind_policy(
                    &ContextKindPolicyDraft {
                        layer: ContextPolicyLayer::Principal,
                        principal: principal.into(),
                        kind: context.kind.clone(),
                        policy,
                    },
                    principal,
                    &context_manager_audit(principal, context.id, "tui_principal_context_policy_updated"),
                )
                .await
        }
        ContextManagerPane::OperatorPolicy => {
            let policy: ContextKindPolicy = serde_json::from_str(value).map_err(parse_error)?;
            store
                .upsert_context_kind_policy(
                    &ContextKindPolicyDraft {
                        layer: ContextPolicyLayer::Operator,
                        principal: String::new(),
                        kind: context.kind.clone(),
                        policy,
                    },
                    principal,
                    &context_manager_audit(principal, context.id, "tui_operator_context_policy_updated"),
                )
                .await
        }
        ContextManagerPane::AnchorOverride => {
            let policy: ContextAnchorPolicy = serde_json::from_str(value).map_err(parse_error)?;
            store
                .upsert_context_anchor_policy(
                    &ContextAnchorPolicyDraft {
                        anchor_context_id: context.id,
                        principal: principal.into(),
                        policy,
                    },
                    principal,
                    &context_manager_audit(principal, context.id, "tui_context_anchor_policy_updated"),
                )
                .await
        }
    }
    .map_err(|error| error.to_string())
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
            let (contexts, context_warning) = match engine.store().get_memory_contexts(&id, principal).await {
                Ok(contexts) => (contexts.iter().map(|membership| ContextDescriptor::from(&membership.context)).collect(), None),
                Err(error) => (Vec::new(), Some(format!("context refresh failed: {error}"))),
            };
            let refresh_warning = [metadata_warning, audit_warning, context_warning].into_iter().flatten().collect::<Vec<_>>().join("; ");
            DataMsg::Updated {
                memory: Box::new(memory),
                metadata,
                audit,
                contexts,
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

fn append_effective_context(context_id: ContextId, included: &mut HashSet<ContextId>, effective: &mut Vec<ContextId>) -> Result<(), String> {
    if effective.len() >= MAX_EFFECTIVE_CONTEXTS {
        return Err(effective_context_limit_message());
    }
    let _inserted = included.insert(context_id);
    effective.push(context_id);
    Ok(())
}

fn effective_context_limit_message() -> String {
    format!("context selection expands beyond the maximum of {MAX_EFFECTIVE_CONTEXTS} effective contexts; narrow the selection or disable descendants")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers},
        style::{Color, Modifier},
    };
    use tokio::sync::mpsc;

    use super::{App, ContextManagerPane, DataMsg, Focus, MAX_EFFECTIVE_CONTEXTS, Mode, MutationEngineFactory, Row, Status, append_effective_context};
    use crate::{
        config::{LimitsConfig, SearchConfig},
        context::{ContextAuditDraft, ContextCreateDraft, ContextId, ContextKind, ContextKindPolicy, ContextKindPolicyDraft, ContextLifecycle, ContextPolicyLayer},
        embedding::{BoxFuture, EmbeddingProvider},
        engine::LocalHoldEngine,
        error::EmbeddingError,
        store::{ContextReader as _, ContextWriter as _, MemoryReader as _, MemoryWriter as _, SqliteStore},
        types::{AccessPolicy, Memory, MemoryId, MemoryUpdate, Provenance, RedactableField, WriteOutcome, normalize_context_key},
        ui::{editor::TextInput, theme::Theme, view},
    };

    struct FixedEmbedding;

    #[test]
    fn local_context_expansion_stops_at_the_effective_ceiling() {
        let mut effective = std::iter::repeat_with(ContextId::new).take(MAX_EFFECTIVE_CONTEXTS).collect::<Vec<_>>();
        let mut included = effective.iter().copied().collect::<HashSet<_>>();
        let error = append_effective_context(ContextId::new(), &mut included, &mut effective).unwrap_err();
        assert!(error.contains("maximum"));
        assert_eq!(effective.len(), MAX_EFFECTIVE_CONTEXTS);
    }

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
        let context_id = create_test_context(&store, "project/test", "Test project").await;
        for content in contents {
            let memory = Memory::new_for_test(
                (*content).to_owned(),
                Vec::new(),
                Provenance::new_for_test(Some("operator".into()), Some("project/test".into()), None),
                AccessPolicy::Public,
            );
            let id = store.store(&memory, None).await.unwrap();
            let _outcome = store
                .replace_memory_contexts(&id, &[context_id], "operator", &ContextAuditDraft {
                    actor_principal: "operator".into(),
                    action: "test_membership".into(),
                    context_id: None,
                    memory_id: Some(id),
                    details: None,
                })
                .await
                .unwrap();
        }
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(engine, Theme::detect(), Some("operator".into()), tx), rx)
    }

    async fn store_governed_test_memory(store: &SqliteStore, memory: &Memory) -> MemoryId {
        let context_id = create_test_context(store, "project/test", "Test project").await;
        let id = store.store(memory, None).await.unwrap();
        let _outcome = store
            .replace_memory_contexts(&id, &[context_id], "operator", &ContextAuditDraft {
                actor_principal: "operator".into(),
                action: "test_membership".into(),
                context_id: None,
                memory_id: Some(id),
                details: None,
            })
            .await
            .unwrap();
        id
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
            contexts: Vec::new(),
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
        memory.superseded_by = Some(MemoryId::new());
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
        let mut terminal = Terminal::new(TestBackend::new(140_u16, 24_u16)).unwrap();
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
        let _id = store_governed_test_memory(&store, &memory).await;
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
        let id = store_governed_test_memory(&store, &memory).await;
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
        let id = store_governed_test_memory(&store, &memory).await;
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
        let id = store_governed_test_memory(&store, &memory).await;
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

    async fn create_test_context(store: &SqliteStore, key: &str, display_name: &str) -> ContextId {
        let id = ContextId::new();
        let _created = store
            .create_context(
                &ContextCreateDraft {
                    id,
                    kind: ContextKind::new(ContextKind::PROJECT).unwrap(),
                    key: key.into(),
                    normalized_key: normalize_context_key(key),
                    display_name: display_name.into(),
                    description: None,
                    owner_principal: "operator".into(),
                    guidance: None,
                    parent_id: None,
                    aliases: Vec::new(),
                    identities: Vec::new(),
                    resolver_hints: Vec::new(),
                    confirm_distinct_from: Vec::new(),
                    enforce_fuzzy_confirmation: false,
                    frozen: false,
                },
                &ContextAuditDraft {
                    actor_principal: "operator".into(),
                    action: "test_context_created".into(),
                    context_id: Some(id),
                    memory_id: None,
                    details: None,
                },
            )
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn stale_context_catalog_is_dropped() {
        let (mut app, _rx) = app_with_memories(&["visible"]).await;
        app.context_generation = 2_u64;
        app.on_data(DataMsg::ContextCatalog {
            records: Vec::new(),
            total: 9_u64,
            warning: None,
            generation: 1_u64,
        });
        assert!(app.contexts.is_empty());
        assert!(app.context_total.is_none());
    }

    #[tokio::test]
    async fn bootstrap_keeps_context_catalog_when_broad_count_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let context_id = create_test_context(&store, "project/catalog-survives", "Catalog survives").await;
        store
            .with_conn(|connection| {
                connection.pragma_update(None, "foreign_keys", false)?;
                let _dropped = connection.execute("DROP TABLE memories", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);

        app.bootstrap().await;

        assert_eq!(app.contexts.len(), 1_usize);
        assert_eq!(app.contexts[0].record.context.id, context_id);
        assert!(app.context_total.is_none());
        assert!(app.context_notice.as_deref().is_some_and(|notice| notice.contains("count unavailable")));
    }

    #[tokio::test]
    async fn context_sidebar_supports_multi_selection_and_broad_search() {
        let store = SqliteStore::in_memory().unwrap();
        let alpha = create_test_context(&store, "project/alpha", "Alpha").await;
        let beta = create_test_context(&store, "project/beta", "Beta").await;
        for (content, key, context_id) in [("alpha memory", "project/alpha", alpha), ("beta memory", "project/beta", beta)] {
            let memory = Memory::new_for_test(
                content.into(),
                Vec::new(),
                Provenance::new_for_test(Some("operator".into()), Some(key.into()), None),
                AccessPolicy::Public,
            );
            let id = store.store(&memory, None).await.unwrap();
            let _outcome = store
                .replace_memory_contexts(&id, &[context_id], "operator", &ContextAuditDraft {
                    actor_principal: "operator".into(),
                    action: "test_membership".into(),
                    context_id: None,
                    memory_id: Some(id),
                    details: None,
                })
                .await
                .unwrap();
        }
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);

        app.bootstrap().await;
        assert_eq!(app.context_total, Some(2_u64));
        assert_eq!(app.contexts.iter().map(|item| item.record.context.display_name.as_str()).collect::<Vec<_>>(), vec![
            "Alpha", "Beta"
        ]);
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.rows.len(), 2_usize);

        app.on_event(press(KeyCode::Left)).await;
        assert_eq!(app.focus, Focus::Contexts);
        app.on_event(press(KeyCode::Down)).await;
        app.on_event(press(KeyCode::Char(' '))).await;
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.selected_context_ids, vec![alpha]);
        assert_eq!(app.rows.len(), 1_usize);
        assert_eq!(app.rows[0].memory.content, "alpha memory");

        app.on_event(press(KeyCode::End)).await;
        app.on_event(press(KeyCode::Char(' '))).await;
        app.on_data(rx.recv().await.unwrap());
        assert_eq!(app.selected_context_ids, vec![alpha, beta]);
        assert_eq!(app.rows.len(), 2_usize);

        let mut terminal = Terminal::new(TestBackend::new(100_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = rendered_text(buffer);
        assert!(rendered.contains("CONTEXTS"));
        assert!(rendered.contains("[x] Alpha"));
        assert!(rendered.contains("[x] Beta"));

        app.on_event(press(KeyCode::Char('x'))).await;
        app.on_data(rx.recv().await.unwrap());
        assert!(app.selected_context_ids.is_empty());
        assert_eq!(app.rows.len(), 2_usize);
    }

    #[tokio::test]
    async fn context_sidebar_traverses_archived_lineage_without_selecting_archived_nodes() {
        let store = SqliteStore::in_memory().unwrap();
        let grandparent = create_test_context(&store, "project/lineage-grandparent", "Lineage grandparent").await;
        let archived_parent = create_test_context(&store, "project/lineage-parent", "Lineage parent").await;
        let child = create_test_context(&store, "project/lineage-child", "Lineage child").await;
        store
            .set_context_parent(
                &archived_parent,
                Some(&grandparent),
                "operator",
                &ContextAuditDraft::new("operator", "test_lineage_parent_set").with_context(archived_parent),
            )
            .await
            .unwrap();
        store
            .set_context_parent(
                &child,
                Some(&archived_parent),
                "operator",
                &ContextAuditDraft::new("operator", "test_lineage_child_set").with_context(child),
            )
            .await
            .unwrap();
        store
            .set_context_lifecycle(
                &archived_parent,
                ContextLifecycle::Archived,
                "operator",
                &ContextAuditDraft::new("operator", "test_lineage_parent_archived").with_context(archived_parent),
            )
            .await
            .unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);

        app.bootstrap().await;
        assert!(app.contexts.iter().all(|item| item.record.context.id != archived_parent));
        app.focus = Focus::Contexts;
        app.context_cursor = app.contexts.iter().position(|item| item.record.context.id == child).unwrap().saturating_add(1);
        app.on_event(press(KeyCode::Char(' '))).await;

        assert_eq!(app.selected_context_ids, vec![child]);
        let effective = app.effective_selected_context_ids().unwrap().into_iter().collect::<HashSet<_>>();
        assert!(effective.contains(&child));
        assert!(effective.contains(&grandparent));
        assert!(!effective.contains(&archived_parent));
    }

    #[tokio::test]
    async fn context_sidebar_rejects_effectively_denied_kind() {
        let store = SqliteStore::in_memory().unwrap();
        let context_id = create_test_context(&store, "project/policy-denied", "Policy denied").await;
        store
            .upsert_context_kind_policy(
                &ContextKindPolicyDraft::new(ContextPolicyLayer::Operator, "", ContextKind::new(ContextKind::PROJECT).unwrap(), ContextKindPolicy {
                    allowed: Some(false),
                    ..ContextKindPolicy::default()
                }),
                "operator",
                &ContextAuditDraft::new("operator", "test_project_selection_denied"),
            )
            .await
            .unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);

        app.bootstrap().await;
        app.focus = Focus::Contexts;
        app.context_cursor = app.contexts.iter().position(|item| item.record.context.id == context_id).unwrap().saturating_add(1);
        app.on_event(press(KeyCode::Char(' '))).await;

        assert!(app.selected_context_ids.is_empty());
        assert!(matches!(&app.status, Status::NotHeld(message) if message.contains("denies context kind project")));
    }

    #[tokio::test]
    async fn context_manager_renders_panes_and_protects_unsaved_changes() {
        let (mut app, _rx) = app_with_memories(&["managed"]).await;
        app.bootstrap().await;
        app.focus = Focus::Contexts;
        app.context_cursor = 1;
        app.on_event(press(KeyCode::Char('c'))).await;
        assert_eq!(app.mode, Mode::ContextManager);

        let mut terminal = Terminal::new(TestBackend::new(180_u16, 28_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let rendered = rendered_text(terminal.backend().buffer());
        assert!(rendered.contains("CONTEXT MANAGER"));
        assert!(rendered.contains("DEFINITION"));
        assert!(rendered.contains("IDENTITIES"));
        assert!(rendered.contains("OPERATOR DEFAULTS"));
        assert!(rendered.contains("RECENT CONTEXT AUDIT"));

        app.on_event(press(KeyCode::Char('e'))).await;
        app.context_manager.as_mut().unwrap().edit.as_mut().unwrap().input = TextInput::new((0_u32..30_u32).map(|line| format!("line-{line}")).collect::<Vec<_>>().join("\n"));
        let mut edit_terminal = Terminal::new(TestBackend::new(60_u16, 10_u16)).unwrap();
        let _completed = edit_terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let rendered_edit = rendered_text(edit_terminal.backend().buffer());
        assert!(rendered_edit.contains("line-29"));
        assert!(rendered_edit.contains('\u{2588}'));
        app.context_manager.as_mut().unwrap().edit.as_mut().unwrap().input.insert(' ');
        app.on_event(press(KeyCode::Esc)).await;
        assert_eq!(app.mode, Mode::ConfirmContextDiscard);
        app.on_event(press(KeyCode::Char('n'))).await;
        assert_eq!(app.mode, Mode::ContextManagerEdit);
        app.on_event(press(KeyCode::Esc)).await;
        app.on_event(press(KeyCode::Char('y'))).await;
        assert_eq!(app.mode, Mode::ContextManager);
        assert!(app.context_manager.as_ref().unwrap().edit.is_none());
    }

    #[tokio::test]
    async fn context_manager_rejects_operator_controls_for_non_operator_principal() {
        let store = SqliteStore::in_memory().unwrap();
        let id = ContextId::new();
        let _created = store
            .create_context(
                &ContextCreateDraft::private(id, ContextKind::new(ContextKind::PROJECT).unwrap(), "project/alice", "Alice", "alice"),
                &ContextAuditDraft::new("alice", "test_alice_context_created").with_context(id),
            )
            .await
            .unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("alice".into()), tx);
        app.bootstrap().await;
        app.focus = Focus::Contexts;
        app.context_cursor = 1;
        app.on_event(press(KeyCode::Char('c'))).await;
        for pane in [ContextManagerPane::OperatorPolicy, ContextManagerPane::Kinds] {
            app.context_manager.as_mut().unwrap().pane = pane;
            app.on_event(press(KeyCode::Char('e'))).await;
            assert_eq!(app.mode, Mode::ContextManager);
            assert!(matches!(&app.status, Status::NotHeld(message) if message.contains("principal operator")));
        }
    }

    #[tokio::test]
    async fn context_manager_archives_and_reactivates_contexts() {
        let (mut app, _rx) = app_with_memories(&["managed"]).await;
        app.bootstrap().await;
        app.focus = Focus::Contexts;
        app.context_cursor = 1;
        app.on_event(press(KeyCode::Char('c'))).await;
        let context_id = app.context_manager.as_ref().unwrap().record.context.id;
        app.selected_context_ids.push(context_id);
        app.context_manager.as_mut().unwrap().pane = ContextManagerPane::Lifecycle;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.context_manager.as_mut().unwrap().edit.as_mut().unwrap().input = TextInput::new("{\"lifecycle\":\"archived\"}".into());
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        assert_eq!(
            app.engine.store().get_context(&context_id, "operator").await.unwrap().unwrap().lifecycle,
            ContextLifecycle::Archived
        );
        assert!(app.selected_context_ids.is_empty(), "archiving must immediately remove a cached retrieval selection");

        app.on_event(press(KeyCode::Char('e'))).await;
        app.context_manager.as_mut().unwrap().edit.as_mut().unwrap().input = TextInput::new("{\"lifecycle\":\"active\"}".into());
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
        assert_eq!(
            app.engine.store().get_context(&context_id, "operator").await.unwrap().unwrap().lifecycle,
            ContextLifecycle::Active
        );
    }

    #[tokio::test]
    async fn archived_context_can_be_reactivated_from_a_fresh_app_catalog() {
        let store = SqliteStore::in_memory().unwrap();
        let context_id = create_test_context(&store, "project/archived", "Archived project").await;
        store
            .set_context_lifecycle(
                &context_id,
                ContextLifecycle::Archived,
                "operator",
                &ContextAuditDraft::new("operator", "test_context_archived").with_context(context_id),
            )
            .await
            .unwrap();
        let engine = LocalHoldEngine::new(store.clone(), Arc::new(FixedEmbedding), LimitsConfig::default(), SearchConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = App::new(engine, Theme::detect(), Some("operator".into()), tx);

        app.bootstrap().await;
        assert!(app.contexts.is_empty(), "active catalog must omit archived contexts");
        app.focus = Focus::Contexts;
        app.on_event(press(KeyCode::Char('a'))).await;
        while app.contexts.is_empty() {
            app.on_data(rx.recv().await.unwrap());
        }
        assert!(app.show_archived_contexts);
        assert_eq!(app.contexts[0].record.context.id, context_id);
        assert_eq!(app.contexts[0].record.context.lifecycle, ContextLifecycle::Archived);

        app.context_cursor = 1;
        app.on_event(press(KeyCode::Char(' '))).await;
        assert!(app.selected_context_ids.is_empty(), "archived contexts cannot become retrieval filters");
        app.on_event(press(KeyCode::Char('c'))).await;
        assert_eq!(app.mode, Mode::ContextManager);
        app.context_manager.as_mut().unwrap().pane = ContextManagerPane::Lifecycle;
        app.on_event(press(KeyCode::Char('e'))).await;
        app.context_manager.as_mut().unwrap().edit.as_mut().unwrap().input = TextInput::new("{\"lifecycle\":\"active\"}".into());
        app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;

        assert_eq!(store.get_context(&context_id, "operator").await.unwrap().unwrap().lifecycle, ContextLifecycle::Active);
    }

    #[tokio::test]
    async fn context_manager_updates_grants_policies_anchor_overrides_and_kinds() {
        let (mut app, _rx) = app_with_memories(&["managed"]).await;
        app.bootstrap().await;
        app.focus = Focus::Contexts;
        app.context_cursor = 1;
        app.on_event(press(KeyCode::Char('c'))).await;
        let context_id = app.context_manager.as_ref().unwrap().record.context.id;

        for (pane, json) in [
            (ContextManagerPane::Grants, "[\"hermes\"]"),
            (ContextManagerPane::PrincipalPolicy, "{\"guidance\":\"principal guidance\"}"),
            (ContextManagerPane::OperatorPolicy, "{\"agent_creation\":true}"),
            (ContextManagerPane::AnchorOverride, "{\"kinds\":{\"project\":{\"include_descendants\":true}}}"),
            (ContextManagerPane::Kinds, "{\"kind\":\"workstream\",\"display_name\":\"Workstream\",\"enabled\":true}"),
        ] {
            app.context_manager.as_mut().unwrap().pane = pane;
            app.on_event(press(KeyCode::Char('e'))).await;
            app.context_manager.as_mut().unwrap().edit.as_mut().unwrap().input = TextInput::new(json.into());
            app.on_event(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
            assert_eq!(app.mode, Mode::ContextManager);
        }

        let grants = app.engine.store().list_context_grants(&context_id, "operator").await.unwrap();
        assert!(grants.iter().any(|grant| grant.grantee_principal == "hermes"));
        let policies = app.engine.store().list_context_kind_policies("operator").await.unwrap();
        assert!(
            policies
                .iter()
                .any(|record| { record.layer == ContextPolicyLayer::Principal && record.policy.guidance.as_deref() == Some("principal guidance") })
        );
        assert!(
            policies
                .iter()
                .any(|record| record.layer == ContextPolicyLayer::Operator && record.policy.agent_creation == Some(true))
        );
        let anchors = app.engine.store().list_context_anchor_policies("operator").await.unwrap();
        assert!(anchors.iter().any(|record| record.anchor_context_id == context_id));
        let kinds = app.engine.store().list_context_kinds().await.unwrap();
        assert!(kinds.iter().any(|definition| definition.kind.as_str() == "workstream"));
    }

    #[tokio::test]
    async fn frame_renders_brand_chrome() {
        let (mut app, mut rx) = app_with_memories(&["the keep stands"]).await;
        app.bootstrap().await;
        app.on_data(rx.recv().await.unwrap());
        app.notice = Some("reranker off: artifacts are not cached".into());
        let mut terminal = Terminal::new(TestBackend::new(140_u16, 24_u16)).unwrap();
        let _completed = terminal.draw(|frame| view::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = rendered_text(buffer);
        assert!(rendered.contains("localhold"), "header should carry the wordmark");
        assert!(
            rendered.contains("mode auto"),
            "the header should name the configured automatic mode before a search executes"
        );
        assert!(rendered.contains("CONTEXTS"), "context pane should be titled");
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
