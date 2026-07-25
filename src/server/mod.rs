//! MCP protocol handlers — tool routing, request dispatch, and response formatting.

/// MCP tool parameter and response types.
pub mod params;

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use axum::http::request::Parts;
use params::{
    AdminBulkDeleteParams, AdminBulkUpdateParams, AdminCleanupExpiredParams, AdminConsolidateParams, AdminCountParams, AdminFilterFields, AdminHistoryParams, AdminListParams,
    AdminListResponse, AdminMigrateMetadataParams, AdminMigrateMetadataResponse, AdminMigrationReportParams, AdminMigrationReportResponse, AdminReassignScopeParams,
    AdminReembedParams, AdminScopeListParams, AdminScopeListResponse, AdminScopeRegisterParams, AdminScopeRegisterResponse, AgentCount, AuditEntryResponse, BriefParams,
    BriefResponse, BulkDeleteResponse, BulkUpdateResponse, ConsolidateResponse, ContextCandidate, ContextCreateParams, ContextCreateResponse, ContextResolution,
    ContextResolveParams, ContextResolveResponse, CountResponse, DeleteResponse, DuplicateCandidateCard, DuplicateGroupEntry, EvictExpiredResponse, ForgetParams, HandoffCandidate,
    HandoffParams, HandoffResponse, HandoffSuggestion, HistoryResponse, InventoryCard, MatchAction, MatchAssessment, MatchDiagnostics, MatchQuality, MatchScoreBasis, MemoryEntry,
    NextAction, OperationStatus, OperationSummary, QualityWarning, QualityWarningSeverity, ReadManyItemResponse, ReadManyParams, ReadManyResponse, ReadManyStatus, ReadParams,
    ReadResponse, ReassignScopeResponse, RecallCard, RecallParams, RecallResponse, RecommendedAction, RecommendedActionPriority, RecommendedActionTool, ReembedResponse,
    RememberManyItemResponse, RememberManyParams, RememberManyResponse, RememberParams, RememberResponse, ReviseParams, ScopeCount, ScopeEntry, ScopeResolution, ScopeResolvedBy,
    TagCount, ToolError, ToolErrorCode, ToolErrorResponse, UpdateResponse,
};
use rmcp::{
    RoleServer, ServerHandler,
    handler::server::{
        tool::{ToolCallContext, ToolRouter},
        wrapper::Parameters,
    },
    model::{CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool},
    service::RequestContext,
    tool, tool_router,
};

use crate::{
    clock::Clock,
    config::{AnonymousPolicy, LimitsConfig, SearchConfig},
    context::{
        ContextAnchorPolicyRecord, ContextAuditDraft, ContextCreateDraft, ContextDescriptor, ContextEnvelope, ContextExactLookup, ContextId, ContextKind, ContextKindDefinition,
        ContextKindPolicyRecord, ContextPolicyLayer, ContextRecord, ContextReference, EffectiveContextPolicy, MAX_CONTEXT_CONFIRMATIONS, MAX_CONTEXT_DESCRIPTION_LEN,
        MAX_CONTEXT_DISPLAY_NAME_LEN, MAX_CONTEXT_HINTS, MAX_CONTEXT_REFS, MAX_CONTEXT_SURFACE_LEN, UNRESOLVED_CONTEXT_KEY as UNRESOLVED_SCOPE, evaluate_context_policy,
        legacy_scope_display_name, normalize_context_identity, validate_implicit_legacy_context_key, validate_legacy_scope_definition, validate_legacy_scope_key,
    },
    embedding::EmbeddingProvider,
    engine::{BulkUpdateFields, LocalHoldEngine, ReembedOutcome, ReembedRequest, SearchRequest, StoreMemoryInput},
    error::EngineError,
    store::{MemoryStore, RecordUseOutcome},
    types::{
        ANONYMOUS_PRINCIPAL, AccessLevel, LARGE_CONTENT_WARNING_THRESHOLD_BYTES, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryUpdate, MetadataPatch, QueryContext,
        RedactableField, ScopeDefinition, WriteOutcome, normalize_context_key,
    },
    validation::{normalize_non_empty, normalize_optional_non_empty, normalize_optional_string_array, validate_batch_len, validate_optional_non_empty},
};

const REDACTED_SCOPE: &str = "[redacted]";
const SERVER_PRINCIPAL: &str = "stdio";
const HTTP_PRINCIPAL: &str = "http";
const READ_EVENT_WEIGHT: f64 = 1.0;

const ADMIN_TOOLS: &[&str] = &[
    "admin_bulk_delete",
    "admin_bulk_update",
    "admin_cleanup_expired",
    "admin_consolidate",
    "admin_count",
    "admin_history",
    "admin_list",
    "admin_reassign_scope",
    "admin_reembed",
    "admin_scope_list",
    "admin_scope_register",
    "admin_migrate_metadata",
    "admin_migration_report",
];

const DEFAULT_DISCOVERY_TOOLS: &[&str] = &[
    "admin_bulk_delete",
    "admin_bulk_update",
    "admin_cleanup_expired",
    "admin_consolidate",
    "admin_count",
    "admin_history",
    "admin_list",
    "admin_reassign_scope",
    "admin_reembed",
    "admin_scope_list",
    "admin_scope_register",
    "admin_migrate_metadata",
    "admin_migration_report",
    "brief",
    "context_create",
    "context_resolve",
    "forget",
    "handoff",
    "read",
    "read_many",
    "recall",
    "remember",
    "remember_many",
    "revise",
];

struct MemoryView {
    memory: Memory,
    metadata: Option<MemoryMetadata>,
    contexts: Vec<ContextDescriptor>,
    primary_context_key: Option<String>,
    has_context_memberships: bool,
}

struct PreparedRemember {
    memory: Memory,
    supersedes: Option<MemoryId>,
    metadata: MemoryMetadata,
    scope_resolution: ScopeResolution,
    context_resolution: ContextResolution,
    direct_context_ids: Vec<ContextId>,
    duplicate_candidates: Vec<DuplicateCandidateCard>,
    warnings: Vec<QualityWarning>,
    created_legacy_context: Option<ContextId>,
}

enum PrepareRememberError {
    Invalid {
        error: crate::error::ValidationError,
        suggested_fix: &'static str,
    },
    Engine(EngineError),
    Tool(CallToolResult),
}

impl PrepareRememberError {
    const fn invalid(error: crate::error::ValidationError, suggested_fix: &'static str) -> Self {
        Self::Invalid { error, suggested_fix }
    }

    fn into_tool_result(self, item_index: Option<usize>) -> Result<CallToolResult, rmcp::ErrorData> {
        match self {
            Self::Invalid { error, suggested_fix } => {
                let field = item_index.map_or_else(|| error.field.clone(), |index| format!("memories[{index}].{}", error.field));
                Ok(tool_error(ToolErrorCode::InvalidParams, Some(&field), error.to_string(), Some(suggested_fix), false))
            }
            Self::Engine(error) => Err(error.into()),
            Self::Tool(result) => Ok(result),
        }
    }
}

enum AdminFilterError {
    Engine(EngineError),
    Tool(CallToolResult),
}

impl From<EngineError> for AdminFilterError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<crate::error::ValidationError> for AdminFilterError {
    fn from(error: crate::error::ValidationError) -> Self {
        Self::Engine(error.into())
    }
}

impl AdminFilterError {
    fn into_tool_result(self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self {
            Self::Engine(EngineError::Validation(error)) => Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some(&error.field),
                error.to_string(),
                Some("Correct the invalid administrative filter and retry."),
                false,
            )),
            Self::Engine(error) => Err(error.into()),
            Self::Tool(result) => Ok(result),
        }
    }
}

#[derive(Debug)]
struct ResolvedContextSelection {
    resolution: ContextResolution,
    direct_ids: Vec<ContextId>,
    effective_ids: Vec<ContextId>,
    created_legacy_context: Option<ContextId>,
}

#[derive(Debug)]
struct ContextPolicyState {
    kinds: Vec<ContextKindDefinition>,
    kind_policies: Vec<ContextKindPolicyRecord>,
    anchor_policies: Vec<ContextAnchorPolicyRecord>,
}

struct PreparedHandoffWrite {
    memory: Memory,
    supersedes: Option<MemoryId>,
    metadata: MemoryMetadata,
    context_ids: Vec<ContextId>,
}

struct PreparedHandoff {
    suggestion: HandoffSuggestion,
    write: Option<PreparedHandoffWrite>,
}

impl MemoryView {
    const fn new(memory: Memory, metadata: Option<MemoryMetadata>, contexts: Vec<ContextDescriptor>, primary_context_key: Option<String>, has_context_memberships: bool) -> Self {
        Self {
            memory,
            metadata,
            contexts,
            primary_context_key,
            has_context_memberships,
        }
    }

    const fn is_redacted(&self) -> bool {
        self.memory.was_redacted
    }

    fn content_visible(&self) -> bool {
        self.memory.field_visible_in_view(&RedactableField::Content)
    }

    fn provenance_visible(&self) -> bool {
        self.memory.field_visible_in_view(&RedactableField::Provenance)
    }

    fn summary(&self) -> Option<String> {
        if !self.content_visible() {
            return None;
        }
        self.metadata.as_ref().and_then(|metadata| metadata.summary.clone())
    }

    fn summary_or_excerpt(&self) -> String {
        self.summary().unwrap_or_else(|| compact_excerpt(&self.memory.content))
    }

    fn scope(&self) -> Option<String> {
        if !self.provenance_visible() {
            return None;
        }
        if let Some(primary_context_key) = &self.primary_context_key {
            return Some(primary_context_key.clone());
        }
        (!self.has_context_memberships).then(|| UNRESOLVED_SCOPE.to_owned())
    }

    fn card_scope(&self) -> String {
        self.scope().unwrap_or_else(|| {
            if self.is_redacted() || self.has_context_memberships {
                REDACTED_SCOPE.to_owned()
            } else {
                UNRESOLVED_SCOPE.to_owned()
            }
        })
    }

    fn unresolved_scope(&self) -> bool {
        self.scope().as_deref() == Some(UNRESOLVED_SCOPE)
    }

    fn agent_label(&self) -> Option<String> {
        if !self.provenance_visible() {
            return None;
        }
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.agent_label.clone())
            .or_else(|| self.memory.provenance.source_agent.clone())
    }

    fn created_by_principal(&self) -> Option<String> {
        if self.is_redacted() {
            return None;
        }
        self.metadata.as_ref().and_then(|metadata| metadata.created_by_principal.clone())
    }

    fn quality_flags(&self) -> Vec<String> {
        if self.is_redacted() {
            Vec::new()
        } else {
            self.metadata.as_ref().map_or_else(Vec::new, |metadata| metadata.quality_flags.clone())
        }
    }

    const fn updated_at_for_wire(&self) -> chrono::DateTime<chrono::Utc> {
        if self.is_redacted() { self.memory.created_at } else { self.memory.updated_at }
    }
}

impl From<EngineError> for rmcp::ErrorData {
    fn from(e: EngineError) -> Self {
        match &e {
            EngineError::Validation(_) => Self::invalid_params(e.to_string(), None),
            EngineError::Store(se) => match se {
                crate::error::StoreError::NotFound(_) => Self::invalid_params(e.to_string(), None),
                crate::error::StoreError::Conflict(_) => Self::internal_error(format!("conflict: {e}"), None),
                crate::error::StoreError::Database(_) | crate::error::StoreError::Serialization(_) | crate::error::StoreError::MigrationFailed { .. } => {
                    Self::internal_error(e.to_string(), None)
                }
            },
            EngineError::ShuttingDown | EngineError::EmbeddingUnavailable(_) | EngineError::SearchUnavailable(_) | EngineError::Embedding(_) | EngineError::Config(_) => {
                Self::internal_error(e.to_string(), None)
            }
        }
    }
}

/// Source of the authenticated identity used by HTTP requests.
///
/// [`Self::Fixed`] is the safe default for shared bearer-token authentication:
/// request headers cannot change the configured identity. [`Self::TrustedProxyHeader`]
/// is only safe when the HTTP endpoint is inaccessible to untrusted clients and
/// the named header is overwritten by a separately authenticated reverse proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpPrincipalSource {
    /// Every valid bearer token resolves to this fixed identity.
    Fixed(String),
    /// Resolve identity from a header asserted by a trusted reverse proxy.
    TrustedProxyHeader(String),
}

impl HttpPrincipalSource {
    /// Configure one fixed identity for all bearer-authenticated HTTP requests.
    pub fn fixed<P: Into<String>>(principal: P) -> Self {
        Self::Fixed(principal.into())
    }

    /// Trust a reverse proxy to assert identity in `header_name`.
    ///
    /// The deployment must prevent direct client access to this endpoint, and
    /// the proxy must remove any client-supplied copy of the identity header.
    pub fn trusted_proxy_header<H: Into<String>>(header_name: H) -> Self {
        Self::TrustedProxyHeader(header_name.into())
    }
}

/// The MCP server for `LocalHold` memory operations.
///
/// Generic over the store backend `S`, which must implement the full
/// [`MemoryStore`] trait (read, write, and admin operations).
#[derive(Clone)]
pub struct LocalHoldServer<S: MemoryStore + Clone + std::fmt::Debug + 'static = crate::store::SqliteStore> {
    engine: LocalHoldEngine<S>,
    tool_router: ToolRouter<Self>,
    principal: Option<Arc<str>>,
    anonymous_policy: AnonymousPolicy,
    http_auth_token: Option<Arc<str>>,
    http_principal_source: HttpPrincipalSource,
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> std::fmt::Debug for LocalHoldServer<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHoldServer").field("engine", &self.engine).finish_non_exhaustive()
    }
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> LocalHoldServer<S> {
    fn standard_tool_router() -> ToolRouter<Self> {
        let mut router = Self::tool_router();
        for name in ADMIN_TOOLS {
            router.remove_route(name);
        }
        router
    }

    /// Remove privileged maintenance tools from discovery and dispatch.
    #[must_use]
    pub fn without_admin_tools(mut self) -> Self {
        self.tool_router = Self::standard_tool_router();
        self
    }

    /// Add privileged maintenance tools to discovery and dispatch.
    ///
    /// Enable these routes only for an operator-controlled instance.
    #[must_use]
    pub fn with_admin_tools(mut self) -> Self {
        self.tool_router = Self::tool_router();
        self
    }

    /// Create a new server with the given store, embedding provider, and operational limits.
    #[must_use]
    pub fn new(store: S, embedding: Arc<dyn EmbeddingProvider>, limits: LimitsConfig, search_config: SearchConfig) -> Self {
        Self {
            engine: LocalHoldEngine::new(store, embedding, limits, search_config),
            tool_router: Self::standard_tool_router(),
            principal: Some(Arc::<str>::from(SERVER_PRINCIPAL)),
            anonymous_policy: AnonymousPolicy::PublicReadOnly,
            http_auth_token: None,
            http_principal_source: HttpPrincipalSource::fixed(HTTP_PRINCIPAL),
        }
    }

    /// Create a server from a pre-built engine (allows sharing the engine with other tasks).
    #[must_use]
    pub fn from_engine(engine: LocalHoldEngine<S>) -> Self {
        Self {
            engine,
            tool_router: Self::standard_tool_router(),
            principal: Some(Arc::<str>::from(SERVER_PRINCIPAL)),
            anonymous_policy: AnonymousPolicy::PublicReadOnly,
            http_auth_token: None,
            http_principal_source: HttpPrincipalSource::fixed(HTTP_PRINCIPAL),
        }
    }

    /// Create a server from a pre-built engine with explicit authorization settings.
    #[must_use]
    pub fn from_engine_with_auth(engine: LocalHoldEngine<S>, principal: Option<String>, anonymous_policy: AnonymousPolicy) -> Self {
        Self {
            engine,
            tool_router: Self::standard_tool_router(),
            principal: principal.map(Arc::<str>::from),
            anonymous_policy,
            http_auth_token: None,
            http_principal_source: HttpPrincipalSource::fixed(HTTP_PRINCIPAL),
        }
    }

    /// Create a server from a pre-built engine with explicit server and HTTP authorization settings.
    ///
    /// Use [`HttpPrincipalSource::TrustedProxyHeader`] only behind a trusted proxy.
    #[must_use]
    pub fn from_engine_with_auth_and_http(
        engine: LocalHoldEngine<S>,
        principal: Option<String>,
        anonymous_policy: AnonymousPolicy,
        http_auth_token: Option<String>,
        http_principal_source: HttpPrincipalSource,
    ) -> Self {
        Self {
            engine,
            tool_router: Self::standard_tool_router(),
            principal: principal.map(Arc::<str>::from),
            anonymous_policy,
            http_auth_token: http_auth_token.map(Arc::<str>::from),
            http_principal_source,
        }
    }

    /// Create a new server with a custom clock (for testing).
    #[must_use]
    pub fn new_with_clock(store: S, embedding: Arc<dyn EmbeddingProvider>, limits: LimitsConfig, search_config: SearchConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            engine: LocalHoldEngine::new_with_clock(store, embedding, limits, search_config, clock),
            tool_router: Self::standard_tool_router(),
            principal: Some(Arc::<str>::from(SERVER_PRINCIPAL)),
            anonymous_policy: AnonymousPolicy::PublicReadOnly,
            http_auth_token: None,
            http_principal_source: HttpPrincipalSource::fixed(HTTP_PRINCIPAL),
        }
    }

    fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    fn configured_mcp_principal(&self) -> Option<&str> {
        self.principal()
            .map(str::trim)
            .filter(|principal| !principal.is_empty() && !crate::http_auth::is_reserved_principal(principal))
    }

    fn caller_context_for(principal: Option<&str>) -> QueryContext {
        QueryContext {
            principal: principal.map(ToOwned::to_owned),
        }
    }

    fn principal_for_context(&self, context: &RequestContext<RoleServer>) -> Option<String> {
        if context.extensions.get::<Parts>().is_some() {
            return self.http_principal_for_context(context);
        }
        self.configured_mcp_principal().map(ToOwned::to_owned)
    }

    fn http_principal_for_context(&self, context: &RequestContext<RoleServer>) -> Option<String> {
        let token = self.http_auth_token.as_deref()?;
        let parts = context.extensions.get::<Parts>()?;
        if !crate::http_auth::bearer_matches(&parts.headers, token) {
            return None;
        }
        match &self.http_principal_source {
            HttpPrincipalSource::Fixed(principal) => {
                let principal = principal.trim();
                (!principal.is_empty()).then(|| principal.to_owned())
            }
            HttpPrincipalSource::TrustedProxyHeader(header_name) => crate::http_auth::trusted_proxy_principal(&parts.headers, header_name).map(ToOwned::to_owned),
        }
    }

    const fn anonymous_read_allowed(&self) -> bool {
        matches!(self.anonymous_policy, AnonymousPolicy::PublicReadOnly | AnonymousPolicy::PublicReadWrite)
    }

    const fn read_allowed_for(&self, principal: Option<&str>) -> bool {
        principal.is_some() || self.anonymous_read_allowed()
    }

    fn write_principal_for(&self, principal: Option<&str>) -> Option<String> {
        principal.map(ToOwned::to_owned).or_else(|| match self.anonymous_policy {
            AnonymousPolicy::PublicReadWrite => Some(ANONYMOUS_PRINCIPAL.to_owned()),
            AnonymousPolicy::PublicReadOnly | AnonymousPolicy::DenyAll => None,
        })
    }

    fn local_admin_error_for_context(&self, context: &RequestContext<RoleServer>) -> Option<CallToolResult> {
        if context.extensions.get::<Parts>().is_some() {
            return Some(Self::local_admin_denied());
        }
        if self.configured_mcp_principal().is_none() {
            return Some(Self::anonymous_write_denied());
        }
        None
    }

    fn anonymous_read_denied() -> CallToolResult {
        tool_error(
            ToolErrorCode::AnonymousReadDenied,
            None,
            "anonymous reads are disabled",
            Some("Configure a trusted principal or enable anonymous public reads."),
            false,
        )
    }

    fn anonymous_write_denied() -> CallToolResult {
        tool_error(
            ToolErrorCode::AnonymousWriteDenied,
            None,
            "anonymous writes are disabled",
            Some("Configure a trusted principal or enable anonymous public writes."),
            false,
        )
    }

    fn local_admin_denied() -> CallToolResult {
        tool_error(
            ToolErrorCode::AccessDenied,
            None,
            "tool requires local server admin context",
            Some("Run this maintenance tool over a trusted local stdio session instead of HTTP principal delegation."),
            false,
        )
    }

    async fn memory_view(&self, memory: Memory, principal: Option<&str>) -> Result<MemoryView, EngineError> {
        self.memory_views(vec![memory], principal)
            .await?
            .pop()
            .ok_or_else(|| EngineError::Store(crate::error::StoreError::Conflict("memory view batch unexpectedly returned no row".into())))
    }

    async fn memory_views(&self, memories: Vec<Memory>, principal: Option<&str>) -> Result<Vec<MemoryView>, EngineError> {
        let memory_ids = memories.iter().map(|memory| memory.id).collect::<Vec<_>>();
        let (metadata, memberships, membership_presence) = tokio::try_join!(
            self.engine.store().get_metadata_batch(&memory_ids),
            self.engine.store().get_memory_contexts_batch(&memory_ids, principal.unwrap_or(ANONYMOUS_PRINCIPAL)),
            self.engine.store().get_memory_context_presence_batch(&memory_ids, principal.unwrap_or(ANONYMOUS_PRINCIPAL)),
        )?;
        Ok(memories
            .into_iter()
            .map(|memory| {
                let visible_memberships = memberships.get(&memory.id).cloned().unwrap_or_default();
                let primary_context_key = (!memory.was_redacted)
                    .then(|| {
                        visible_memberships
                            .iter()
                            .find(|membership| membership.ordinal == 0)
                            .map(|membership| membership.context.key.clone())
                    })
                    .flatten();
                let contexts = if memory.was_redacted {
                    Vec::new()
                } else {
                    visible_memberships.iter().map(|membership| ContextDescriptor::from(&membership.context)).collect()
                };
                let memory_metadata = metadata.get(&memory.id).cloned();
                let has_context_memberships = membership_presence.contains(&memory.id);
                MemoryView::new(memory, memory_metadata, contexts, primary_context_key, has_context_memberships)
            })
            .collect())
    }

    fn recall_card_from_memory_view(result: &crate::types::SearchResult, view: MemoryView, reranker_blend_weight: f64) -> RecallCard {
        let r#match = match_assessment(result, reranker_blend_weight);
        let diagnostics = if result.memory.was_redacted {
            MatchDiagnostics::default()
        } else {
            match_diagnostics(result, reranker_blend_weight)
        };
        recall_card_from_view(view, r#match, diagnostics)
    }

    fn duplicate_card_from_memory_view(result: &crate::types::SearchResult, view: &MemoryView, reranker_blend_weight: f64) -> DuplicateCandidateCard {
        let r#match = match_assessment(result, reranker_blend_weight);
        DuplicateCandidateCard {
            id: view.memory.id,
            summary_or_excerpt: view.summary_or_excerpt(),
            r#match,
        }
    }

    fn full_read_item_from_memory_view(id: MemoryId, view: MemoryView, activity_recorded: bool) -> ReadManyItemResponse {
        let summary = view.summary();
        let scope = view.scope();
        let agent_label = view.agent_label();
        let created_by_principal = view.created_by_principal();
        let quality_flags = view.quality_flags();
        let unresolved_scope = view.unresolved_scope();
        let contexts = view.contexts.clone();
        ReadManyItemResponse {
            id,
            status: ReadManyStatus::Found,
            memory: Some(MemoryEntry::from(view.memory.sanitize_for_wire())),
            summary,
            scope,
            contexts,
            agent_label,
            created_by_principal,
            quality_flags,
            unresolved_scope,
            activity_recorded,
        }
    }

    #[expect(clippy::too_many_lines, reason = "scope-resolution diagnostics are clearer when the ordered resolution branches stay together")]
    async fn resolve_scope(&self, principal: &str, explicit_scope: Option<String>, context_hints: &[String]) -> Result<ScopeResolution, EngineError> {
        let explicit_scope = normalize_legacy_scope_value("scope", explicit_scope).map_err(EngineError::from)?;
        if explicit_scope.is_none() && context_hints.is_empty() {
            return Ok(ScopeResolution {
                scope: UNRESOLVED_SCOPE.to_owned(),
                unresolved_scope: true,
                resolved_by: ScopeResolvedBy::Unresolved,
                matched_hint: None,
                matched_value: None,
            });
        }
        let registry = self.engine.list_scopes_for_principal(principal).await?;
        if let Some(scope) = explicit_scope {
            if let Some(entry) = registry.iter().find(|entry| entry.scope_key == scope) {
                return Ok(ScopeResolution {
                    scope: entry.scope_key.clone(),
                    unresolved_scope: false,
                    resolved_by: ScopeResolvedBy::Explicit,
                    matched_hint: None,
                    matched_value: Some(entry.scope_key.clone()),
                });
            }
            if let Some((entry, alias)) = registry
                .iter()
                .find_map(|entry| entry.aliases.iter().find(|alias| *alias == &scope).map(|alias| (entry, alias)))
            {
                return Ok(ScopeResolution {
                    scope: entry.scope_key.clone(),
                    unresolved_scope: false,
                    resolved_by: ScopeResolvedBy::Alias,
                    matched_hint: Some(scope),
                    matched_value: Some(alias.clone()),
                });
            }
            if let Some((entry, matcher)) = registry
                .iter()
                .find_map(|entry| entry.matchers.iter().find(|matcher| scope.contains(*matcher)).map(|matcher| (entry, matcher)))
            {
                return Ok(ScopeResolution {
                    scope: entry.scope_key.clone(),
                    unresolved_scope: false,
                    resolved_by: ScopeResolvedBy::Matcher,
                    matched_hint: Some(scope),
                    matched_value: Some(matcher.clone()),
                });
            }
            return Ok(ScopeResolution {
                scope,
                unresolved_scope: false,
                resolved_by: ScopeResolvedBy::Explicit,
                matched_hint: None,
                matched_value: None,
            });
        }
        if let Some((entry, hint, matcher)) = registry.iter().find_map(|entry| {
            entry
                .matchers
                .iter()
                .find_map(|matcher| context_hints.iter().find(|hint| hint.contains(matcher)).map(|hint| (entry, hint, matcher)))
        }) {
            return Ok(ScopeResolution {
                scope: entry.scope_key.clone(),
                unresolved_scope: false,
                resolved_by: ScopeResolvedBy::Matcher,
                matched_hint: Some(hint.clone()),
                matched_value: Some(matcher.clone()),
            });
        }
        Ok(ScopeResolution {
            scope: UNRESOLVED_SCOPE.to_owned(),
            unresolved_scope: true,
            resolved_by: ScopeResolvedBy::Unresolved,
            matched_hint: context_hints.first().cloned(),
            matched_value: None,
        })
    }

    async fn authorized_context_records(&self, principal: &str) -> Result<Vec<ContextRecord>, EngineError> {
        let mut records = Vec::new();
        loop {
            let page = self.engine.store().list_context_records(principal, false, records.len(), 500).await?;
            let page_len = page.len();
            records.extend(page);
            if page_len < 500 {
                break;
            }
        }
        Ok(records)
    }

    async fn context_policy_state(&self, principal: &str) -> Result<ContextPolicyState, EngineError> {
        let (kinds, kind_policies, anchor_policies) = tokio::try_join!(
            self.engine.store().list_context_kinds(),
            self.engine.store().list_context_kind_policies(principal),
            self.engine.store().list_context_anchor_policies(principal),
        )?;
        Ok(ContextPolicyState {
            kinds,
            kind_policies,
            anchor_policies,
        })
    }

    fn effective_policy_for(state: &ContextPolicyState, records: &[ContextRecord], kind: &ContextKind, active_context_ids: &[ContextId]) -> EffectiveContextPolicy {
        let operator = state
            .kind_policies
            .iter()
            .find(|record| record.layer == ContextPolicyLayer::Operator && &record.kind == kind)
            .map(|record| &record.policy);
        let principal = state
            .kind_policies
            .iter()
            .find(|record| record.layer == ContextPolicyLayer::Principal && &record.kind == kind)
            .map(|record| &record.policy);
        let active = active_context_ids.iter().copied().collect::<HashSet<_>>();
        let mut anchor_records = state
            .anchor_policies
            .iter()
            .filter(|record| active.contains(&record.anchor_context_id) && record.policy.kinds.contains_key(kind.as_str()))
            .collect::<Vec<_>>();
        let anchor_ids = anchor_records.iter().map(|record| record.anchor_context_id).collect::<Vec<_>>();
        anchor_records.retain(|candidate| {
            !anchor_ids
                .iter()
                .any(|other_id| *other_id != candidate.anchor_context_id && context_is_ancestor(records, candidate.anchor_context_id, *other_id))
        });
        let anchor_policies = anchor_records.iter().filter_map(|record| record.policy.kinds.get(kind.as_str())).collect::<Vec<_>>();
        let mut effective = evaluate_context_policy(kind, operator, principal, &anchor_policies);
        if state.kinds.iter().find(|definition| &definition.kind == kind).is_none_or(|definition| !definition.enabled) {
            effective.allowed = false;
            effective.guidance.push(format!("Context kind {kind} is disabled by the operator."));
        }
        effective
    }

    fn policy_guidance_for(effective: impl IntoIterator<Item = EffectiveContextPolicy>) -> Vec<String> {
        let mut guidance = context_policy_guidance();
        for policy in effective {
            for item in policy.guidance {
                push_unique(&mut guidance, item);
            }
            for ambiguity in policy.ambiguities {
                let item = format!("Policy ambiguity for {}: {ambiguity}", policy.kind);
                push_unique(&mut guidance, item);
            }
        }
        guidance
    }

    fn context_resolution_error(code: ToolErrorCode, field: &str, message: impl Into<String>, candidates: Vec<ContextCandidate>) -> CallToolResult {
        Self::context_resolution_error_with_guidance(code, field, message, candidates, context_policy_guidance())
    }

    fn context_expansion_error(field: &'static str, error: crate::error::StoreError) -> CallToolResult {
        match error {
            crate::error::StoreError::NotFound(message) | crate::error::StoreError::Conflict(message) => tool_error(
                ToolErrorCode::Conflict,
                Some(field),
                message,
                Some("Resolve active authorized contexts and retry without unavailable or cyclic hierarchy members."),
                false,
            ),
            crate::error::StoreError::Database(_) | crate::error::StoreError::Serialization(_) | crate::error::StoreError::MigrationFailed { .. } => tool_error(
                ToolErrorCode::Internal,
                Some(field),
                "context hierarchy expansion failed in the persistence backend",
                Some("Retry after checking backend health."),
                true,
            ),
        }
    }

    #[expect(clippy::needless_pass_by_value, reason = "structured tool errors take ownership of candidates and guidance")]
    fn context_resolution_error_with_guidance(
        code: ToolErrorCode,
        field: &str,
        message: impl Into<String>,
        candidates: Vec<ContextCandidate>,
        policy_guidance: Vec<String>,
    ) -> CallToolResult {
        let recommended_actions = vec![RecommendedAction {
            tool: RecommendedActionTool::ContextResolve,
            priority: RecommendedActionPriority::High,
            reason: "Resolve an authorized governed context before retrying the operation.".to_owned(),
            arguments: Some(serde_json::json!({
                "context": {
                    "refs": [],
                    "hints": [],
                    "include_descendants": false,
                    "allow_unresolved": false
                },
                "query": null,
                "offset": 0_i32,
                "limit": 20_i32
            })),
        }];
        tool_error_with_details(
            code,
            Some(field),
            message,
            Some("Call context_resolve, then retry with an exact context ID, key, or typed identity."),
            false,
            Some(serde_json::json!({
                "candidates": candidates,
                "policy_guidance": policy_guidance,
                "retry": {
                    "context": {
                        "refs": [],
                        "hints": [],
                        "include_descendants": false,
                        "allow_unresolved": false
                    },
                    "selected_candidate_id": null
                }
            })),
            recommended_actions,
        )
    }

    #[expect(clippy::result_large_err, reason = "protocol validation returns the shared structured MCP error body")]
    fn exact_context_lookup(reference: &ContextReference) -> Result<ContextExactLookup, CallToolResult> {
        if let Err(message) = reference.validate() {
            return Err(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context.refs"),
                message,
                Some("Use exactly one of id, kind+key, or kind+identity in each context ref."),
                false,
            ));
        }
        if let Some(id) = reference.id {
            return Ok(ContextExactLookup::Id(id));
        }
        let Some(kind) = reference.kind.as_ref() else {
            return Err(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context.refs"),
                "context kind is required for key and identity locators",
                Some("Use exactly one of id, kind+key, or kind+identity."),
                false,
            ));
        };
        if let Some(key) = reference.key.as_deref() {
            return Ok(ContextExactLookup::Key {
                kind: Some(kind.clone()),
                normalized_key: normalize_context_key(key),
            });
        }
        let Some(identity_input) = reference.identity.as_ref() else {
            return Err(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context.refs.identity"),
                "context identity is required for an identity locator",
                Some("Use exactly one of id, kind+key, or kind+identity."),
                false,
            ));
        };
        let identity = normalize_context_identity(identity_input).map_err(|message| {
            tool_error(
                ToolErrorCode::InvalidParams,
                Some("context.refs.identity"),
                message,
                Some("Supply a policy-approved durable identity; local paths belong in context.hints."),
                false,
            )
        })?;
        Ok(ContextExactLookup::Identity { kind: kind.clone(), identity })
    }

    async fn exact_context_records(&self, principal: &str, include_archived: bool, reference: &ContextReference) -> Result<Vec<ContextRecord>, CallToolResult> {
        let lookup = Self::exact_context_lookup(reference)?;
        self.engine.store().find_context_records(principal, include_archived, &lookup).await.map_err(|error| {
            tool_error(
                ToolErrorCode::Internal,
                Some("context"),
                error.to_string(),
                Some("Retry after checking backend health."),
                true,
            )
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "ordered exact, hint, ambiguity, hierarchy, and broad-search resolution is one protocol operation"
    )]
    #[expect(clippy::excessive_nesting, reason = "resolution intentionally follows exact, hint, default, policy, and hierarchy precedence")]
    async fn resolve_context_selection(
        &self,
        principal: &str,
        envelope: Option<&ContextEnvelope>,
        query: Option<&str>,
        governed_write: bool,
    ) -> Result<ResolvedContextSelection, CallToolResult> {
        let omitted = envelope.is_none() && query.is_none();
        let envelope = envelope.cloned().unwrap_or_default();
        if let Err(message) = envelope.validate_limits() {
            return Err(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context"),
                message,
                Some("Reduce context locators and hints to the documented limits."),
                false,
            ));
        }
        if query.is_some_and(|query| query.len() > MAX_CONTEXT_SURFACE_LEN) {
            return Err(tool_error(
                ToolErrorCode::InvalidParams,
                Some("query"),
                format!("context query accepts at most {MAX_CONTEXT_SURFACE_LEN} bytes"),
                Some("Use a shorter context key, name, or hint."),
                false,
            ));
        }
        let policy_state = self.context_policy_state(principal).await.map_err(|error| {
            tool_error(
                ToolErrorCode::Internal,
                Some("context"),
                error.to_string(),
                Some("Retry after checking backend health."),
                true,
            )
        })?;
        if omitted && !governed_write {
            let policies = policy_state
                .kinds
                .iter()
                .map(|definition| Self::effective_policy_for(&policy_state, &[], &definition.kind, &[]))
                .collect::<Vec<_>>();
            return Ok(ResolvedContextSelection {
                resolution: ContextResolution {
                    broad_search: true,
                    policy_guidance: Self::policy_guidance_for(policies),
                    ..ContextResolution::default()
                },
                direct_ids: Vec::new(),
                effective_ids: Vec::new(),
                created_legacy_context: None,
            });
        }

        let mut direct_ids = Vec::new();
        let mut resolved_records = BTreeMap::<ContextId, ContextRecord>::new();
        let mut candidates = Vec::new();
        let mut unresolved_explicit_ref = false;
        let mut unresolved_key_refs = Vec::new();

        for reference in &envelope.refs {
            let matches = self.exact_context_records(principal, false, reference).await?;
            match matches.as_slice() {
                [record] => {
                    if !direct_ids.contains(&record.context.id) {
                        direct_ids.push(record.context.id);
                    }
                    let _previous = resolved_records.insert(record.context.id, record.clone());
                }
                [] => {
                    unresolved_explicit_ref = true;
                    if let Some(key) = reference.key.as_deref() {
                        unresolved_key_refs.push((key.to_owned(), reference.kind.clone()));
                    }
                }
                _ => {
                    let ambiguous = matches
                        .iter()
                        .map(|record| ContextCandidate {
                            context: ContextDescriptor::from(&record.context),
                            score: 1.0,
                            matched_by: vec!["ambiguous_exact_locator".to_owned()],
                        })
                        .collect();
                    return Err(Self::context_resolution_error(
                        ToolErrorCode::ContextAmbiguous,
                        "context.refs",
                        "context locator matched multiple authorized contexts",
                        ambiguous,
                    ));
                }
            }
        }

        let mut lookup_values = envelope.hints.clone();
        if let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) {
            lookup_values.push(query.to_owned());
        }
        let mut unresolved_lookup_values = Vec::new();
        for value in &lookup_values {
            let exact = self
                .engine
                .store()
                .find_context_records(principal, false, &ContextExactLookup::Key {
                    kind: None,
                    normalized_key: normalize_context_key(value),
                })
                .await
                .map_err(|error| {
                    tool_error(
                        ToolErrorCode::Internal,
                        Some("context"),
                        error.to_string(),
                        Some("Retry after checking backend health."),
                        true,
                    )
                })?;
            match exact.as_slice() {
                [record] => {
                    if !direct_ids.contains(&record.context.id) {
                        direct_ids.push(record.context.id);
                    }
                    let _previous = resolved_records.insert(record.context.id, record.clone());
                }
                [] => unresolved_lookup_values.push(value.clone()),
                _ => {
                    let ambiguous = exact
                        .iter()
                        .map(|record| ContextCandidate {
                            context: ContextDescriptor::from(&record.context),
                            score: 1.0,
                            matched_by: vec!["ambiguous_alias_or_hint".to_owned()],
                        })
                        .collect();
                    return Err(Self::context_resolution_error(
                        ToolErrorCode::ContextAmbiguous,
                        "context.hints",
                        "context alias or hint matched multiple authorized contexts",
                        ambiguous,
                    ));
                }
            }
        }
        let fuzzy_records = if unresolved_key_refs.is_empty() && unresolved_lookup_values.is_empty() {
            Vec::new()
        } else {
            self.authorized_context_records(principal).await.map_err(|error| {
                tool_error(
                    ToolErrorCode::Internal,
                    Some("context"),
                    error.to_string(),
                    Some("Retry after checking backend health."),
                    true,
                )
            })?
        };
        for (key, kind) in unresolved_key_refs {
            candidates.extend(fuzzy_context_candidates(&fuzzy_records, &key, kind.as_ref()));
        }
        for value in unresolved_lookup_values {
            let normalized = normalize_context_key(&value);
            let hint_matches = fuzzy_records
                .iter()
                .filter(|record| {
                    record.hints.iter().any(|hint| {
                        let normalized_hint = normalize_context_key(hint);
                        !normalized_hint.is_empty() && normalized.contains(&normalized_hint)
                    })
                })
                .collect::<Vec<_>>();
            match hint_matches.as_slice() {
                [record] => {
                    if !direct_ids.contains(&record.context.id) {
                        direct_ids.push(record.context.id);
                    }
                    let _previous = resolved_records.insert(record.context.id, (*record).clone());
                }
                [] => candidates.extend(fuzzy_context_candidates(&fuzzy_records, &value, None)),
                _ => {
                    let ambiguous = hint_matches
                        .into_iter()
                        .map(|record| ContextCandidate {
                            context: ContextDescriptor::from(&record.context),
                            score: 1.0,
                            matched_by: vec!["ambiguous_hint".to_owned()],
                        })
                        .collect();
                    return Err(Self::context_resolution_error(
                        ToolErrorCode::ContextAmbiguous,
                        "context.hints",
                        "context hint matched multiple authorized contexts",
                        ambiguous,
                    ));
                }
            }
        }
        candidates.sort_by(|left, right| right.score.total_cmp(&left.score).then_with(|| left.context.id.cmp(&right.context.id)));
        candidates.dedup_by_key(|candidate| candidate.context.id);
        candidates.truncate(MAX_CONTEXT_CONFIRMATIONS);

        if unresolved_explicit_ref {
            let policies = policy_state
                .kinds
                .iter()
                .map(|definition| Self::effective_policy_for(&policy_state, &[], &definition.kind, &[]))
                .collect::<Vec<_>>();
            return Err(Self::context_resolution_error_with_guidance(
                if candidates.is_empty() {
                    ToolErrorCode::ContextRequired
                } else {
                    ToolErrorCode::ContextAmbiguous
                },
                "context.refs",
                "one or more explicit context references did not resolve uniquely",
                candidates,
                Self::policy_guidance_for(policies),
            ));
        }

        if direct_ids.is_empty() {
            let policies = policy_state
                .kinds
                .iter()
                .map(|definition| Self::effective_policy_for(&policy_state, &[], &definition.kind, &[]))
                .collect::<Vec<_>>();
            let policy_guidance = Self::policy_guidance_for(policies.clone());
            if policies.iter().any(|policy| !policy.ambiguities.is_empty()) {
                return Err(Self::context_resolution_error_with_guidance(
                    ToolErrorCode::ContextAmbiguous,
                    "context",
                    "effective context policy is ambiguous",
                    candidates,
                    policy_guidance,
                ));
            }
            let required_by_policy = policies.iter().any(|policy| policy.required);
            if governed_write && envelope.allow_unresolved && !required_by_policy {
                return Ok(ResolvedContextSelection {
                    resolution: ContextResolution {
                        candidates,
                        policy_guidance,
                        unresolved: true,
                        ..ContextResolution::default()
                    },
                    direct_ids,
                    effective_ids: Vec::new(),
                    created_legacy_context: None,
                });
            }
            if governed_write && envelope.refs.is_empty() && envelope.hints.is_empty() && query.is_none() {
                let mut missing_required = Vec::new();
                for policy in &policies {
                    if !policy.allowed {
                        if policy.required {
                            missing_required.push(format!("required kind {} is denied", policy.kind));
                        }
                        continue;
                    }
                    if let Some(default_id) = policy.default_context_id {
                        let defaults = self
                            .engine
                            .store()
                            .find_context_records(principal, false, &ContextExactLookup::Id(default_id))
                            .await
                            .map_err(|error| {
                                tool_error(
                                    ToolErrorCode::Internal,
                                    Some("context"),
                                    error.to_string(),
                                    Some("Retry after checking backend health."),
                                    true,
                                )
                            })?;
                        if let [record] = defaults.as_slice()
                            && record.context.kind == policy.kind
                            && !direct_ids.contains(&default_id)
                        {
                            direct_ids.push(default_id);
                            let _previous = resolved_records.insert(default_id, record.clone());
                        } else if policy.required {
                            missing_required.push(format!("required kind {} has no authorized active default", policy.kind));
                        }
                    } else if policy.required {
                        missing_required.push(format!("required kind {} has no default", policy.kind));
                    }
                }
                if !missing_required.is_empty() {
                    return Err(Self::context_resolution_error_with_guidance(
                        ToolErrorCode::ContextRequired,
                        "context",
                        missing_required.join("; "),
                        candidates,
                        policy_guidance,
                    ));
                }
            }
            if direct_ids.is_empty() && query.is_some() && !governed_write {
                let recommended_actions = vec![RecommendedAction {
                    tool: RecommendedActionTool::ContextResolve,
                    priority: RecommendedActionPriority::High,
                    reason: "Select one exact authorized candidate, or create a distinct context when policy permits.".into(),
                    arguments: None,
                }];
                return Ok(ResolvedContextSelection {
                    resolution: ContextResolution {
                        candidates,
                        policy_guidance,
                        unresolved: true,
                        recommended_actions,
                        ..ContextResolution::default()
                    },
                    direct_ids,
                    effective_ids: Vec::new(),
                    created_legacy_context: None,
                });
            }
            if direct_ids.is_empty() {
                if governed_write || !envelope.refs.is_empty() || !envelope.hints.is_empty() {
                    return Err(Self::context_resolution_error_with_guidance(
                        if candidates.is_empty() {
                            ToolErrorCode::ContextRequired
                        } else {
                            ToolErrorCode::ContextAmbiguous
                        },
                        "context",
                        "no unique authorized context could be selected",
                        candidates,
                        policy_guidance,
                    ));
                }
                return Ok(ResolvedContextSelection {
                    resolution: ContextResolution {
                        broad_search: true,
                        policy_guidance,
                        ..ContextResolution::default()
                    },
                    direct_ids,
                    effective_ids: Vec::new(),
                    created_legacy_context: None,
                });
            }
        }

        let initial_effective = self
            .engine
            .store()
            .expand_context_selection(&direct_ids, principal, false)
            .await
            .map_err(|error| Self::context_expansion_error("context", error))?;
        let initial_effective_ids = initial_effective.iter().map(|context| context.id).collect::<Vec<_>>();
        let policy_records = initial_effective
            .iter()
            .cloned()
            .map(|context| ContextRecord {
                context,
                aliases: Vec::new(),
                identities: Vec::new(),
                hints: Vec::new(),
            })
            .collect::<Vec<_>>();
        let direct_kinds = direct_ids
            .iter()
            .filter_map(|id| resolved_records.get(id))
            .map(|record| record.context.kind.clone())
            .collect::<HashSet<_>>();
        let policies = policy_state
            .kinds
            .iter()
            .map(|definition| Self::effective_policy_for(&policy_state, &policy_records, &definition.kind, &initial_effective_ids))
            .collect::<Vec<_>>();
        let policy_guidance = Self::policy_guidance_for(policies.clone());
        if let Some(policy) = policies.iter().find(|policy| !policy.ambiguities.is_empty()) {
            return Err(Self::context_resolution_error_with_guidance(
                ToolErrorCode::ContextAmbiguous,
                "context",
                format!("effective policy for {} has conflicting active-anchor defaults", policy.kind),
                candidates,
                policy_guidance,
            ));
        }
        if let Some(policy) = policies.iter().find(|policy| direct_kinds.contains(&policy.kind) && !policy.allowed) {
            return Err(Self::context_resolution_error_with_guidance(
                ToolErrorCode::AccessDenied,
                "context",
                format!("effective policy denies context kind {}", policy.kind),
                candidates,
                policy_guidance,
            ));
        }
        if governed_write && let Some(policy) = policies.iter().find(|policy| policy.required && !direct_kinds.contains(&policy.kind)) {
            return Err(Self::context_resolution_error_with_guidance(
                ToolErrorCode::ContextRequired,
                "context",
                format!("effective policy requires a {} context", policy.kind),
                candidates,
                policy_guidance,
            ));
        }
        for policy in policies.iter().filter(|policy| direct_kinds.contains(&policy.kind)) {
            if let Some(allowed_companions) = &policy.allowed_companion_kinds
                && let Some(disallowed) = direct_kinds
                    .iter()
                    .find(|candidate_kind| *candidate_kind != &policy.kind && !allowed_companions.contains(candidate_kind))
            {
                return Err(Self::context_resolution_error_with_guidance(
                    ToolErrorCode::Conflict,
                    "context",
                    format!("effective {} policy does not allow companion kind {disallowed}", policy.kind),
                    candidates,
                    policy_guidance,
                ));
            }
        }
        let include_descendants = envelope.include_descendants || policies.iter().any(|policy| direct_kinds.contains(&policy.kind) && policy.include_descendants);
        let effective = if include_descendants {
            self.engine
                .store()
                .expand_context_selection(&direct_ids, principal, true)
                .await
                .map_err(|error| Self::context_expansion_error("context", error))?
        } else {
            initial_effective
        };
        let direct = direct_ids
            .iter()
            .filter_map(|id| resolved_records.get(id))
            .map(|record| ContextDescriptor::from(&record.context))
            .collect();
        let effective_ids = effective.iter().map(|context| context.id).collect();
        Ok(ResolvedContextSelection {
            resolution: ContextResolution {
                direct,
                effective: effective.iter().map(ContextDescriptor::from).collect(),
                candidates,
                policy_guidance,
                broad_search: false,
                unresolved: false,
                recommended_actions: Vec::new(),
            },
            direct_ids,
            effective_ids,
            created_legacy_context: None,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "legacy adaptation includes exact reuse, race recovery, hierarchy linkage, and governed resolution"
    )]
    #[expect(clippy::excessive_nesting, reason = "legacy adaptation preserves ordered compatibility and race-recovery branches")]
    async fn resolve_legacy_context_selection(
        &self,
        principal: &str,
        scope_resolution: &ScopeResolution,
        governed_write: bool,
    ) -> Result<ResolvedContextSelection, CallToolResult> {
        let normalized = normalize_context_key(&scope_resolution.scope);
        if normalized == normalize_context_key(UNRESOLVED_SCOPE) && scope_resolution.resolved_by == ScopeResolvedBy::Explicit {
            let message = if governed_write {
                "inbox/unresolved is a compatibility label, not a governed context; explicitly defer with context.allow_unresolved=true"
            } else {
                "inbox/unresolved is a compatibility label for contextless memories and cannot be used as a governed context filter"
            };
            return Err(Self::context_resolution_error(ToolErrorCode::ContextRequired, "scope", message, Vec::new()));
        }
        if scope_resolution.unresolved_scope {
            if !governed_write {
                let mut selection = self.resolve_context_selection(principal, None, None, false).await?;
                selection.resolution.unresolved = true;
                return Ok(selection);
            }
            return Err(Self::context_resolution_error(
                ToolErrorCode::ContextRequired,
                "scope",
                "legacy scope hints did not resolve; select a governed context or explicitly defer with context.allow_unresolved=true",
                Vec::new(),
            ));
        }
        let matches = self
            .engine
            .store()
            .find_context_records(principal, false, &ContextExactLookup::Key {
                kind: None,
                normalized_key: normalized.clone(),
            })
            .await
            .map_err(|error| {
                tool_error(
                    ToolErrorCode::Internal,
                    Some("scope"),
                    error.to_string(),
                    Some("Retry after checking backend health."),
                    true,
                )
            })?;
        let mut created_legacy_context = None;
        let id = match matches.as_slice() {
            [record] => record.context.id,
            [_, _, ..] => {
                let candidates = matches
                    .iter()
                    .map(|record| ContextCandidate {
                        context: ContextDescriptor::from(&record.context),
                        score: 1.0,
                        matched_by: vec!["legacy_scope".to_owned()],
                    })
                    .collect();
                return Err(Self::context_resolution_error(
                    ToolErrorCode::ContextAmbiguous,
                    "scope",
                    "legacy scope matched multiple visible governed contexts",
                    candidates,
                ));
            }
            [] if !governed_write => {
                let records = self.authorized_context_records(principal).await.map_err(|error| {
                    tool_error(
                        ToolErrorCode::Internal,
                        Some("scope"),
                        error.to_string(),
                        Some("Retry after checking backend health."),
                        true,
                    )
                })?;
                return Err(Self::context_resolution_error(
                    ToolErrorCode::ContextRequired,
                    "scope",
                    "explicit legacy scope does not resolve to an active authorized context",
                    fuzzy_context_candidates(&records, &scope_resolution.scope, None),
                ));
            }
            [] => {
                if let Err(message) = validate_implicit_legacy_context_key(&scope_resolution.scope) {
                    return Err(tool_error(
                        ToolErrorCode::InvalidParams,
                        Some("scope"),
                        message,
                        Some("Use a legacy scope key of at most 512 bytes whose final path segment is at most 256 bytes."),
                        false,
                    ));
                }
                let policy_state = self.context_policy_state(principal).await.map_err(|error| {
                    tool_error(
                        ToolErrorCode::Internal,
                        Some("scope"),
                        error.to_string(),
                        Some("Retry after checking context-policy storage."),
                        true,
                    )
                })?;
                let custom_kind = ContextKind::custom();
                let explicit_custom_policies = policy_state.kind_policies.iter().filter(|record| record.kind == custom_kind).collect::<Vec<_>>();
                let effective_policy = Self::effective_policy_for(&policy_state, &[], &custom_kind, &[]);
                let policy_guidance = Self::policy_guidance_for([effective_policy.clone()]);
                if !effective_policy.allowed || explicit_custom_policies.iter().any(|record| record.policy.agent_creation == Some(false)) {
                    return Err(tool_error_with_details(
                        ToolErrorCode::AccessDenied,
                        Some("scope"),
                        "effective policy denies implicit legacy custom-context creation",
                        Some("Select an existing context or ask an operator to adjust the explicit custom-context creation policy."),
                        false,
                        Some(serde_json::json!({ "policy_guidance": policy_guidance })),
                        Vec::new(),
                    ));
                }
                if explicit_custom_policies.iter().any(|record| record.policy.require_identity == Some(true)) {
                    return Err(tool_error_with_details(
                        ToolErrorCode::ContextRequired,
                        Some("scope"),
                        "effective policy requires a durable identity and legacy scope cannot supply one",
                        Some("Use context_create with a durable identity, then retry the write with a context reference."),
                        false,
                        Some(serde_json::json!({ "policy_guidance": policy_guidance })),
                        Vec::new(),
                    ));
                }
                let id = ContextId::new();
                let draft = ContextCreateDraft {
                    id,
                    kind: ContextKind::custom(),
                    key: scope_resolution.scope.clone(),
                    normalized_key: normalized.clone(),
                    display_name: legacy_scope_display_name(&scope_resolution.scope),
                    description: Some("Private compatibility context created from a legacy scope input.".to_owned()),
                    owner_principal: principal.to_owned(),
                    guidance: Some("Migrate callers from scope to the shared context envelope.".to_owned()),
                    parent_id: None,
                    aliases: Vec::new(),
                    identities: Vec::new(),
                    resolver_hints: Vec::new(),
                    confirm_distinct_from: Vec::new(),
                    enforce_fuzzy_confirmation: false,
                    frozen: false,
                };
                let audit = ContextAuditDraft {
                    actor_principal: principal.to_owned(),
                    action: "legacy_scope_context_created".to_owned(),
                    context_id: Some(id),
                    memory_id: None,
                    details: Some(serde_json::json!({ "scope_key": scope_resolution.scope })),
                };
                match self.engine.store().create_context(&draft, &audit).await {
                    Ok(_created) => {
                        created_legacy_context = Some(id);
                        id
                    }
                    Err(error) => {
                        let exact = self
                            .engine
                            .store()
                            .find_context_records(principal, false, &ContextExactLookup::Key {
                                kind: None,
                                normalized_key: normalized,
                            })
                            .await
                            .map_err(|refresh_error| {
                                tool_error(
                                    ToolErrorCode::Internal,
                                    Some("scope"),
                                    refresh_error.to_string(),
                                    Some("Retry after checking backend health."),
                                    true,
                                )
                            })?
                            .into_iter()
                            .map(|record| record.context.id)
                            .collect::<Vec<_>>();
                        match exact.as_slice() {
                            [existing] => *existing,
                            _ => {
                                return Err(tool_error(
                                    ToolErrorCode::Conflict,
                                    Some("scope"),
                                    error.to_string(),
                                    Some("Resolve the scope again; another caller may have created its compatibility context."),
                                    true,
                                ));
                            }
                        }
                    }
                }
            }
        };
        let envelope = ContextEnvelope {
            refs: vec![ContextReference {
                id: Some(id),
                ..ContextReference::default()
            }],
            ..ContextEnvelope::default()
        };
        let mut selection = match self.resolve_context_selection(principal, Some(&envelope), None, governed_write).await {
            Ok(selection) => selection,
            Err(error) => {
                if let Err(cleanup_error) = self.rollback_created_legacy_context(created_legacy_context, principal).await {
                    return Err(tool_error(
                        ToolErrorCode::Internal,
                        Some("scope"),
                        cleanup_error.to_string(),
                        Some("Retry after checking backend health; the compatibility context may require operator cleanup."),
                        true,
                    ));
                }
                return Err(error);
            }
        };
        selection.created_legacy_context = created_legacy_context;
        Ok(selection)
    }

    async fn resolve_admin_context_id(&self, principal: &str, context_id: ContextId) -> Result<ResolvedContextSelection, AdminFilterError> {
        let envelope = ContextEnvelope {
            refs: vec![ContextReference {
                id: Some(context_id),
                ..ContextReference::default()
            }],
            ..ContextEnvelope::default()
        };
        self.resolve_context_selection(principal, Some(&envelope), None, false)
            .await
            .map_err(AdminFilterError::Tool)
    }

    async fn resolve_direct_admin_legacy_context(&self, principal: &str, value: String) -> Result<ResolvedContextSelection, AdminFilterError> {
        let exact_matches = self
            .engine
            .store()
            .find_context_records(principal, false, &ContextExactLookup::Key {
                kind: None,
                normalized_key: normalize_context_key(&value),
            })
            .await
            .map_err(EngineError::from)?;
        match exact_matches.as_slice() {
            [exact] => self.resolve_admin_context_id(principal, exact.context.id).await,
            [] => {
                let resolution = self.resolve_scope(principal, Some(value), &[]).await.map_err(AdminFilterError::Engine)?;
                self.resolve_legacy_context_selection(principal, &resolution, false).await.map_err(AdminFilterError::Tool)
            }
            _ => Err(AdminFilterError::Tool(Self::context_resolution_error(
                ToolErrorCode::ContextAmbiguous,
                "scope",
                "legacy scope resolves to multiple governed contexts",
                exact_matches
                    .iter()
                    .map(|record| ContextCandidate {
                        context: ContextDescriptor::from(&record.context),
                        score: 1.0,
                        matched_by: vec!["legacy_scope".into()],
                    })
                    .collect(),
            ))),
        }
    }

    async fn resolve_generated_admin_ancestor(&self, principal: &str, value: &str) -> Result<Option<ResolvedContextSelection>, AdminFilterError> {
        let generated_matches = self
            .engine
            .store()
            .find_context_records(principal, false, &ContextExactLookup::Key {
                kind: None,
                normalized_key: normalize_context_key(value),
            })
            .await
            .map_err(EngineError::from)?;
        match generated_matches.as_slice() {
            [generated] => self.resolve_admin_context_id(principal, generated.context.id).await.map(Some),
            [] => Ok(None),
            _ => Err(AdminFilterError::Tool(Self::context_resolution_error(
                ToolErrorCode::ContextAmbiguous,
                "scope",
                "generated ancestor resolves to multiple governed contexts",
                generated_matches
                    .iter()
                    .map(|record| ContextCandidate {
                        context: ContextDescriptor::from(&record.context),
                        score: 1.0,
                        matched_by: vec!["legacy_scope_ancestor".into()],
                    })
                    .collect(),
            ))),
        }
    }

    async fn resolve_admin_legacy_context_ids(&self, principal: &str, mut values: Vec<String>, expand_legacy_scopes: bool) -> Result<Vec<ContextId>, AdminFilterError> {
        let directly_requested = values.iter().map(|value| normalize_context_key(value)).collect::<HashSet<_>>();
        if expand_legacy_scopes {
            values = expand_scope_keys(&values).map_err(EngineError::from)?;
        }
        let mut ids = Vec::new();
        let mut seen_ids = HashSet::new();
        for value in values {
            let selection = if directly_requested.contains(&normalize_context_key(&value)) {
                Some(self.resolve_direct_admin_legacy_context(principal, value).await?)
            } else {
                self.resolve_generated_admin_ancestor(principal, &value).await?
            };
            let Some(selection) = selection else {
                continue;
            };
            ids.extend(selection.effective_ids.into_iter().filter(|id| seen_ids.insert(*id)));
        }
        if ids.is_empty() {
            return Err(AdminFilterError::Tool(Self::context_resolution_error(
                ToolErrorCode::ContextRequired,
                "scope",
                "legacy scope filter resolved to no governed contexts",
                Vec::new(),
            )));
        }
        Ok(ids)
    }

    async fn common_filter_from_admin(
        &self,
        fields: AdminFilterFields,
        principal: Option<&str>,
        expand_legacy_scopes: bool,
    ) -> Result<params::CommonFilterFields, AdminFilterError> {
        reject_removed_admin_field(fields.deprecated_source_agent.is_some(), "source_agent", "agent_label")?;
        reject_removed_admin_field(fields.deprecated_source_conversation.is_some(), "source_conversation", "scope")?;
        reject_removed_admin_field(fields.deprecated_origin_conversation.is_some(), "origin_conversation", "origin_scope")?;
        reject_removed_admin_field(fields.deprecated_scope_keys_any.is_some(), "scope_keys_any", "scopes")?;
        let normalized_scope = normalize_legacy_scope_value("scope", fields.scope).map_err(EngineError::from)?;
        let normalized_scopes = normalize_legacy_scope_values("scopes", fields.scopes).map_err(EngineError::from)?;
        if fields.context.is_some() && (normalized_scope.is_some() || !normalized_scopes.is_empty()) {
            return Err(crate::error::ValidationError::new("context", "context cannot be combined with legacy scope or scopes").into());
        }
        let resolution_principal = principal.unwrap_or(ANONYMOUS_PRINCIPAL);
        let (context_ids, legacy_context_ids_any, explicit_context_filter) = if let Some(envelope) = fields.context.as_ref() {
            let ids = self
                .resolve_context_selection(resolution_principal, Some(envelope), None, false)
                .await
                .map_err(AdminFilterError::Tool)?
                .effective_ids;
            let explicit = !ids.is_empty();
            (Some(ids), None, explicit)
        } else if !normalized_scopes.is_empty() {
            let mut values = normalized_scope.into_iter().collect::<Vec<_>>();
            values.extend(normalized_scopes);
            let ids = self.resolve_admin_legacy_context_ids(resolution_principal, values, expand_legacy_scopes).await?;
            (None, Some(ids), true)
        } else if let Some(scope) = normalized_scope {
            let ids = self.resolve_admin_legacy_context_ids(resolution_principal, vec![scope], expand_legacy_scopes).await?;
            (Some(ids), None, true)
        } else {
            (Some(Vec::new()), None, false)
        };
        let agent_label = normalize_optional_non_empty("agent_label", fields.agent_label).map_err(EngineError::from)?;
        let origin_scope = normalize_legacy_scope_value("origin_scope", fields.origin_scope).map_err(EngineError::from)?;
        Ok(params::CommonFilterFields {
            tags: fields.tags,
            agent_label,
            scope: None,
            origin_scope,
            scopes_any: None,
            context_ids,
            legacy_context_ids_any,
            explicit_context_filter,
            principal: principal.map(ToOwned::to_owned),
            memory_type: fields.memory_type,
            include_superseded: fields.include_superseded,
            entity: fields.entity,
            entity_type: fields.entity_type,
        })
    }

    async fn duplicate_candidates(&self, content: &str, context_ids: Option<Vec<ContextId>>, principal: Option<&str>) -> Result<Vec<DuplicateCandidateCard>, EngineError> {
        if context_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(Vec::new());
        }
        let filter = MemoryFilter {
            context_ids,
            explicit_context_filter: true,
            ..MemoryFilter::default()
        };
        let outcome = self
            .engine
            .search_memories(SearchRequest {
                query: compact_excerpt(content),
                limit: 3,
                filter,
                ctx: Self::caller_context_for(principal),
                max_distance: None,
                keywords: None,
                search_mode: Some(crate::types::SearchMode::Auto),
                context: None,
            })
            .await?;
        let views = self.memory_views(outcome.results.iter().map(|result| result.memory.clone()).collect(), principal).await?;
        let mut cards = Vec::with_capacity(outcome.results.len());
        let reranker_blend_weight = self.engine.search_config().reranker.blend_weight;
        for (result, view) in outcome.results.iter().zip(views) {
            cards.push(Self::duplicate_card_from_memory_view(result, &view, reranker_blend_weight));
        }
        Ok(cards)
    }

    #[expect(clippy::too_many_lines, reason = "remember preparation validates and resolves one complete public write contract")]
    async fn prepare_remember(&self, params: RememberParams, principal: String, now: chrono::DateTime<chrono::Utc>) -> Result<PreparedRemember, PrepareRememberError> {
        let summary = trim_optional_text(params.summary);
        let agent_label = trim_optional_text(params.agent_label);
        let context_hints = normalize_legacy_context_hints(params.context_hints).map_err(|error| PrepareRememberError::invalid(error, "Use bounded non-blank context_hints."))?;
        if params.context.is_some() && (params.scope.is_some() || !context_hints.is_empty()) {
            return Err(PrepareRememberError::Tool(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context"),
                "context cannot be combined with legacy scope or context_hints",
                Some("Use the shared context envelope alone, or use only legacy scope/context_hints."),
                false,
            )));
        }
        let memory_input = params::MemoryInput {
            content: params.content,
            tags: params.tags,
            source_agent: Some(principal.clone()),
            source_conversation: Some(UNRESOLVED_SCOPE.to_owned()),
            origin_conversation: Some(UNRESOLVED_SCOPE.to_owned()),
            source_user: None,
            ttl_seconds: None,
            access_policy: params.access_policy,
            memory_type: params.memory_type,
            importance: params.importance,
            confidence: params.confidence,
            supersedes: None,
            entities: params.entities,
        };
        let input = StoreMemoryInput::try_from(memory_input).map_err(|error| PrepareRememberError::invalid(error, "Provide valid memory content and metadata."))?;
        let supersedes = input.supersedes;
        let mut memory = self.engine.build_memory(input, now).map_err(|error| match error {
            EngineError::Validation(error) => PrepareRememberError::invalid(error, "Provide valid memory content and metadata."),
            engine_error @ (EngineError::Config(_)
            | EngineError::Store(_)
            | EngineError::EmbeddingUnavailable(_)
            | EngineError::SearchUnavailable(_)
            | EngineError::Embedding(_)
            | EngineError::ShuttingDown) => PrepareRememberError::Engine(engine_error),
        })?;
        let (context_selection, scope_resolution, used_legacy_adapter) = if let Some(envelope) = params.context.as_ref() {
            let selection = self
                .resolve_context_selection(&principal, Some(envelope), None, true)
                .await
                .map_err(PrepareRememberError::Tool)?;
            let scope = selection
                .resolution
                .direct
                .first()
                .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), |context| context.key.clone());
            let unresolved = selection.direct_ids.is_empty();
            (
                selection,
                ScopeResolution {
                    unresolved_scope: unresolved,
                    scope,
                    resolved_by: if unresolved { ScopeResolvedBy::Unresolved } else { ScopeResolvedBy::Explicit },
                    matched_hint: None,
                    matched_value: None,
                },
                false,
            )
        } else if params.scope.is_some() || !context_hints.is_empty() {
            let scope_resolution = self.resolve_scope(&principal, params.scope, &context_hints).await.map_err(PrepareRememberError::Engine)?;
            let selection = self
                .resolve_legacy_context_selection(&principal, &scope_resolution, true)
                .await
                .map_err(PrepareRememberError::Tool)?;
            let scope_resolution = canonicalize_legacy_scope_resolution(scope_resolution, &selection);
            (selection, scope_resolution, true)
        } else {
            let selection = self.resolve_context_selection(&principal, None, None, true).await.map_err(PrepareRememberError::Tool)?;
            let scope = selection
                .resolution
                .direct
                .first()
                .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), |context| context.key.clone());
            (
                selection,
                ScopeResolution {
                    unresolved_scope: scope == UNRESOLVED_SCOPE,
                    scope,
                    resolved_by: ScopeResolvedBy::Explicit,
                    matched_hint: None,
                    matched_value: Some("policy_default".into()),
                },
                false,
            )
        };
        let scope = scope_resolution.scope.clone();
        let unresolved_scope = scope_resolution.unresolved_scope;
        memory.provenance.source_conversation = Some(scope.clone());
        memory.provenance.origin_conversation = Some(scope.clone());
        let mut warnings = write_quality_warnings(&memory.content, summary.as_deref(), unresolved_scope, &memory.tags, memory.entities.len());
        if used_legacy_adapter {
            warnings.push(quality_warning(
                "legacy_scope_adapter",
                "legacy scope input was adapted to a private governed context; migrate this caller to the context envelope",
            ));
        }
        let duplicate_candidates = self
            .duplicate_candidates(&memory.content, Some(context_selection.effective_ids.clone()), Some(&principal))
            .await
            .unwrap_or_default();
        if !duplicate_candidates.is_empty() {
            warnings.push(quality_warning(
                "duplicate_candidate",
                "similar memories already exist; review duplicate_candidates before relying on this write",
            ));
        }
        let quality_flags = warnings.iter().map(|warning| warning.code.clone()).collect();
        let metadata = MemoryMetadata {
            memory_id: memory.id,
            scope_key: Some(scope),
            summary,
            agent_label,
            created_by_principal: Some(principal),
            quality_flags,
            schema_version: 1,
        };
        Ok(PreparedRemember {
            memory,
            supersedes,
            metadata,
            scope_resolution,
            context_resolution: context_selection.resolution,
            direct_context_ids: context_selection.direct_ids,
            duplicate_candidates,
            warnings,
            created_legacy_context: context_selection.created_legacy_context,
        })
    }

    async fn rollback_created_legacy_contexts(&self, context_ids: &mut Vec<ContextId>, principal: &str) -> Result<(), EngineError> {
        let mut seen = HashSet::new();
        while let Some(context_id) = context_ids.pop() {
            if seen.insert(context_id) {
                let _removed = self.engine.store().rollback_unreferenced_legacy_context(&context_id, principal).await?;
            }
        }
        Ok(())
    }

    async fn rollback_created_legacy_context(&self, context_id: Option<ContextId>, principal: &str) -> Result<(), EngineError> {
        let mut context_ids = context_id.into_iter().collect::<Vec<_>>();
        self.rollback_created_legacy_contexts(&mut context_ids, principal).await
    }

    #[expect(clippy::result_large_err, reason = "prevalidation preserves the shared structured tool error without losing retry details")]
    fn prevalidate_remember(&self, params: &RememberParams, principal: &str, now: chrono::DateTime<chrono::Utc>) -> Result<(), PrepareRememberError> {
        let memory_input = params::MemoryInput {
            content: params.content.clone(),
            tags: params.tags.clone(),
            source_agent: Some(principal.to_owned()),
            source_conversation: Some(UNRESOLVED_SCOPE.to_owned()),
            origin_conversation: Some(UNRESOLVED_SCOPE.to_owned()),
            source_user: None,
            ttl_seconds: None,
            access_policy: params.access_policy.clone(),
            memory_type: params.memory_type,
            importance: params.importance,
            confidence: params.confidence,
            supersedes: None,
            entities: params.entities.clone(),
        };
        let input = StoreMemoryInput::try_from(memory_input).map_err(|error| PrepareRememberError::invalid(error, "Provide valid memory content and metadata."))?;
        self.engine.build_memory(input, now).map(|_memory| ()).map_err(|error| match error {
            EngineError::Validation(error) => PrepareRememberError::invalid(error, "Provide valid memory content and metadata."),
            engine_error @ (EngineError::Config(_)
            | EngineError::Store(_)
            | EngineError::EmbeddingUnavailable(_)
            | EngineError::SearchUnavailable(_)
            | EngineError::Embedding(_)
            | EngineError::ShuttingDown) => PrepareRememberError::Engine(engine_error),
        })
    }

    /// Drain all in-flight background tasks (embedding generation).
    /// Times out after [`LimitsConfig::shutdown_timeout_secs`] to prevent
    /// indefinite hangs on unresponsive providers.
    pub async fn shutdown(&self) {
        self.engine.shutdown().await;
    }

    /// Return the number of in-flight background tasks.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn tracked_task_count(&self) -> usize {
        self.engine.tracked_task_count()
    }

    /// Drain completed tasks and return how many were reaped.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn reap_completed_tasks_for_test(&self) -> usize {
        self.engine.reap_completed_tasks_for_test()
    }

    /// Shut down with a custom timeout (for tests).
    #[cfg(any(test, feature = "testing"))]
    pub async fn shutdown_for_test(&self, timeout: std::time::Duration) {
        self.engine.shutdown_for_test(timeout).await;
    }

    /// Borrow the underlying store (needed for legacy-row seeding in tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn store(&self) -> &S {
        self.engine.store()
    }
}

fn success_json<T: serde::Serialize>(val: &T) -> Result<CallToolResult, rmcp::ErrorData> {
    let json = serde_json::to_string(val).map_err(|e| rmcp::ErrorData::internal_error(format!("failed to serialize response: {e}"), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
}

fn tool_error(code: ToolErrorCode, field: Option<&str>, message: impl Into<String>, suggested_fix: Option<&str>, retryable: bool) -> CallToolResult {
    tool_error_with_details(code, field, message, suggested_fix, retryable, None, Vec::new())
}

#[expect(
    clippy::too_many_arguments,
    reason = "structured tool errors carry stable code, field, guidance, retry state, diagnostics, and actions"
)]
fn tool_error_with_details(
    code: ToolErrorCode,
    field: Option<&str>,
    message: impl Into<String>,
    suggested_fix: Option<&str>,
    retryable: bool,
    details: Option<serde_json::Value>,
    recommended_actions: Vec<RecommendedAction>,
) -> CallToolResult {
    let response = ToolErrorResponse {
        error: ToolError {
            code,
            field: field.map(ToOwned::to_owned),
            message: message.into(),
            suggested_fix: suggested_fix.map(ToOwned::to_owned),
            retryable,
            details,
            recommended_actions,
        },
    };
    let text =
        serde_json::to_string(&response).unwrap_or_else(|err| format!(r#"{{"error":{{"code":"internal","message":"failed to serialize tool error: {err}","retryable":false}}}}"#));
    CallToolResult::error(vec![ContentBlock::text(text)])
}

fn batch_len_tool_error(field_name: &str, len: usize, max_batch_size: usize, suggested_fix: &'static str) -> Option<CallToolResult> {
    validate_batch_len(field_name, len, max_batch_size)
        .err()
        .map(|err| tool_error(ToolErrorCode::InvalidParams, Some(&err.field), err.message, Some(suggested_fix), false))
}

fn quality_warning(code: &str, message: impl Into<String>) -> QualityWarning {
    let (severity, field, suggested_fix) = match code {
        "missing_scope" => (
            QualityWarningSeverity::ActionRequired,
            Some("scope"),
            Some("Provide an explicit scope or context_hints that match a registered scope."),
        ),
        "duplicate_candidate" => (
            QualityWarningSeverity::Warning,
            Some("content"),
            Some("Review duplicate_candidates before relying on this write."),
        ),
        "missing_summary" => (
            QualityWarningSeverity::Info,
            Some("summary"),
            Some("Provide a compact durable summary when the original content is verbose."),
        ),
        "empty_tags" => (
            QualityWarningSeverity::Info,
            Some("tags"),
            Some("Add stable classification tags when they will improve later retrieval."),
        ),
        "empty_entities" => (
            QualityWarningSeverity::Info,
            Some("entities"),
            Some("Attach important people, projects, or artifacts as typed entities."),
        ),
        "oversized_content" => (
            QualityWarningSeverity::Warning,
            Some("content"),
            Some("Store concise durable context instead of a large transcript or source dump."),
        ),
        "possible_code_dump" => (
            QualityWarningSeverity::Warning,
            Some("content"),
            Some("Remember durable rationale or decisions instead of source text."),
        ),
        "unresolved_scope" => (
            QualityWarningSeverity::Warning,
            Some("context_hints"),
            Some("Register a matching scope or pass an explicit scope key."),
        ),
        "empty_brief" => (
            QualityWarningSeverity::Info,
            Some("query"),
            Some("Broaden the query or scope, or add memories for this context."),
        ),
        "contextless_maintenance_scope" => (
            QualityWarningSeverity::Warning,
            Some("context"),
            Some("Supply an explicit context filter when memories without an active context must be excluded from the maintenance operation."),
        ),
        _ => (QualityWarningSeverity::Warning, None, None),
    };
    QualityWarning {
        code: code.to_owned(),
        severity,
        field: field.map(ToOwned::to_owned),
        message: message.into(),
        suggested_fix: suggested_fix.map(ToOwned::to_owned),
    }
}

fn next_action_for_warnings(warnings: &[QualityWarning]) -> NextAction {
    if warnings.iter().any(|warning| warning.code == "duplicate_candidate") {
        NextAction::ReviewDuplicates
    } else if warnings.iter().any(|warning| warning.code == "missing_scope") {
        NextAction::ClassifyScope
    } else if warnings.is_empty() {
        NextAction::None
    } else {
        NextAction::ReviewWarnings
    }
}

const fn recommended_action_priority_rank(priority: RecommendedActionPriority) -> u8 {
    match priority {
        RecommendedActionPriority::High => 0,
        RecommendedActionPriority::Normal => 1,
        RecommendedActionPriority::Low => 2,
    }
}

fn sort_recommended_actions(actions: &mut [RecommendedAction]) {
    actions.sort_by_key(|action| recommended_action_priority_rank(action.priority));
}

fn context_policy_guidance() -> Vec<String> {
    vec![
        "Contexts are relevance metadata and never grant memory access.".to_owned(),
        "Agent creation is enabled by default only for project contexts with a durable typed identity.".to_owned(),
        "Domain, organization, custom, and operator-defined context creation requires TUI policy opt-in.".to_owned(),
    ]
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn context_is_ancestor(records: &[ContextRecord], ancestor_id: ContextId, descendant_id: ContextId) -> bool {
    let by_id = records.iter().map(|record| (record.context.id, record.context.parent_id)).collect::<BTreeMap<_, _>>();
    let mut cursor = by_id.get(&descendant_id).copied().flatten();
    let mut visited = HashSet::new();
    while let Some(context_id) = cursor {
        if context_id == ancestor_id {
            return true;
        }
        if !visited.insert(context_id) {
            return false;
        }
        cursor = by_id.get(&context_id).copied().flatten();
    }
    false
}

fn normalized_tokens(value: &str) -> HashSet<String> {
    normalize_context_key(value)
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn trigrams(value: &str) -> HashSet<String> {
    let normalized = normalize_context_key(value);
    let chars = normalized.chars().collect::<Vec<_>>();
    if chars.len() < 3 {
        return (!normalized.is_empty()).then_some(normalized).into_iter().collect();
    }
    chars.windows(3).map(|window| window.iter().collect()).collect()
}

#[expect(clippy::float_arithmetic, reason = "duplicate protection uses bounded similarity ratios")]
#[expect(clippy::cast_precision_loss, clippy::as_conversions, reason = "context surface sets are bounded by validated context lengths")]
fn set_overlap(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    let denominator = left.len().max(right.len());
    if denominator == 0 {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    intersection as f64 / denominator as f64
}

fn context_text_similarity(left_tokens: &HashSet<String>, left_trigrams: &HashSet<String>, right: &str) -> f64 {
    set_overlap(left_tokens, &normalized_tokens(right)).max(set_overlap(left_trigrams, &trigrams(right)))
}

#[expect(clippy::float_arithmetic, reason = "candidate tie handling compares bounded similarity scores")]
fn record_similarity(record: &ContextRecord, query_tokens: &HashSet<String>, query_trigrams: &HashSet<String>) -> (f64, Vec<String>) {
    let surfaces = std::iter::once(("key", record.context.key.as_str()))
        .chain(std::iter::once(("display_name", record.context.display_name.as_str())))
        .chain(record.aliases.iter().map(|alias| ("alias", alias.as_str())));
    let mut best = 0.0_f64;
    let mut matched_by = Vec::new();
    for (surface, value) in surfaces {
        let score = context_text_similarity(query_tokens, query_trigrams, value);
        if score > best {
            best = score;
            matched_by.clear();
            matched_by.push(surface.to_owned());
        } else if (score - best).abs() < f64::EPSILON && score > 0.0_f64 {
            matched_by.push(surface.to_owned());
        }
    }
    (best, matched_by)
}

fn fuzzy_context_candidates(records: &[ContextRecord], query: &str, kind: Option<&ContextKind>) -> Vec<ContextCandidate> {
    let query_tokens = normalized_tokens(query);
    let query_trigrams = trigrams(query);
    let mut candidates = records
        .iter()
        .filter(|record| kind.is_none_or(|kind| &record.context.kind == kind))
        .filter_map(|record| {
            let (score, matched_by) = record_similarity(record, &query_tokens, &query_trigrams);
            (score >= 0.72_f64).then(|| ContextCandidate {
                context: ContextDescriptor::from(&record.context),
                score,
                matched_by,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.context.key.cmp(&right.context.key))
            .then_with(|| left.context.id.cmp(&right.context.id))
    });
    candidates.truncate(5);
    candidates
}

fn brief_recommended_actions(suggested_reads: &[MemoryId], unresolved_context_hints: bool, no_matches: bool, stale_only_query: Option<&str>) -> Vec<RecommendedAction> {
    let mut actions = Vec::new();
    match suggested_reads {
        [id] => actions.push(RecommendedAction {
            tool: RecommendedActionTool::Read,
            priority: RecommendedActionPriority::High,
            reason: "One relevant memory is worth reading in full.".to_owned(),
            arguments: Some(serde_json::json!({ "id": id })),
        }),
        [_, ..] => actions.push(RecommendedAction {
            tool: RecommendedActionTool::ReadMany,
            priority: RecommendedActionPriority::High,
            reason: "Several relevant memories are worth reading in full.".to_owned(),
            arguments: Some(serde_json::json!({ "ids": suggested_reads })),
        }),
        [] => {}
    }

    if unresolved_context_hints {
        actions.push(RecommendedAction {
            tool: RecommendedActionTool::AdminScopeRegister,
            priority: RecommendedActionPriority::High,
            reason: "Context hints did not resolve to a registered scope.".to_owned(),
            arguments: None,
        });
    }

    if no_matches {
        actions.push(RecommendedAction {
            tool: RecommendedActionTool::Remember,
            priority: RecommendedActionPriority::Normal,
            reason: "No relevant or stale memories matched this brief.".to_owned(),
            arguments: None,
        });
    }

    if let Some(query) = stale_only_query {
        actions.push(RecommendedAction {
            tool: RecommendedActionTool::Recall,
            priority: RecommendedActionPriority::Low,
            reason: "Only weak or stale candidates matched; inspect weak recall results if needed.".to_owned(),
            arguments: Some(serde_json::json!({
                "query": query,
                "include_weak": true
            })),
        });
    }

    sort_recommended_actions(&mut actions);
    actions
}

const fn operation_summary(status: OperationStatus, changed: u64, warnings: Vec<QualityWarning>, next_action: NextAction) -> OperationSummary {
    OperationSummary {
        status,
        changed,
        matched: None,
        denied: None,
        capped: false,
        next_action,
        warnings,
        affected: Vec::new(),
    }
}

fn compact_excerpt(content: &str) -> String {
    let trimmed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&trimmed, 240)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut out = String::new();
    for ch in value.chars().take(max_chars.saturating_sub(3)) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn trim_optional_text(value: Option<String>) -> Option<String> {
    value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

const fn finite_unit_score(value: f64) -> f64 {
    if value.is_finite() { value.clamp(0.0_f64, 1.0_f64) } else { 0.0_f64 }
}

#[expect(clippy::float_arithmetic, reason = "agent-facing relevance blend mirrors ranking relevance formula")]
fn match_score(result: &crate::types::SearchResult, reranker_blend_weight: f64) -> f64 {
    if let Some(breakdown) = result.score_breakdown {
        return finite_unit_score(breakdown.query_relevance);
    }
    let retrieval_score = result.retrieval_score.unwrap_or(0.0_f64);
    let score = result.reranker_score.map_or(retrieval_score, |reranker_score| {
        reranker_blend_weight.mul_add(reranker_score, (1.0_f64 - reranker_blend_weight) * retrieval_score)
    });
    finite_unit_score(score)
}

const fn match_score_basis(result: &crate::types::SearchResult) -> MatchScoreBasis {
    if result.reranker_score.is_some() {
        MatchScoreBasis::RerankerBlend
    } else if result.retrieval_score.is_some() {
        MatchScoreBasis::Retrieval
    } else {
        MatchScoreBasis::Unavailable
    }
}

fn match_quality(score: f64) -> MatchQuality {
    if score >= 0.50_f64 {
        MatchQuality::Strong
    } else if score >= 0.20_f64 {
        MatchQuality::Possible
    } else {
        MatchQuality::Weak
    }
}

const fn match_action(quality: MatchQuality) -> MatchAction {
    match quality {
        MatchQuality::Strong => MatchAction::Read,
        MatchQuality::Possible => MatchAction::Consider,
        MatchQuality::Weak => MatchAction::Ignore,
    }
}

fn match_assessment(result: &crate::types::SearchResult, reranker_blend_weight: f64) -> MatchAssessment {
    let score = match_score(result, reranker_blend_weight);
    let quality = match_quality(score);
    MatchAssessment {
        quality,
        action: match_action(quality),
        score,
        score_basis: match_score_basis(result),
    }
}

fn match_diagnostics(result: &crate::types::SearchResult, reranker_blend_weight: f64) -> MatchDiagnostics {
    MatchDiagnostics {
        retrieval_score: result.retrieval_score,
        reranker_score: result.reranker_score,
        reranker_blend_weight: result.reranker_score.map(|_| reranker_blend_weight),
        vector_distance: result.distance,
        ranking_score: result.composite_score,
    }
}

fn recall_card_from_view(view: MemoryView, r#match: MatchAssessment, diagnostics: MatchDiagnostics) -> RecallCard {
    let summary_or_excerpt = view.summary_or_excerpt();
    let scope = view.card_scope();
    let agent_label = view.agent_label();
    let updated_at = view.updated_at_for_wire();
    let contexts = view.contexts;
    let memory = view.memory;
    RecallCard {
        id: memory.id,
        summary_or_excerpt,
        scope,
        contexts,
        agent_label,
        created_at: memory.created_at.to_rfc3339(),
        updated_at: updated_at.to_rfc3339(),
        tags: memory.tags,
        entities: memory.entities,
        r#match,
        diagnostics,
    }
}

fn inventory_card_from_view(view: MemoryView, now: chrono::DateTime<chrono::Utc>) -> InventoryCard {
    let summary_or_excerpt = view.summary_or_excerpt();
    let scope = view.card_scope();
    let agent_label = view.agent_label();
    let unresolved_scope = view.unresolved_scope();
    let quality_flags = view.quality_flags();
    let updated_at = view.updated_at_for_wire();
    let contexts = view.contexts;
    let memory = view.memory;
    let expired = memory.expires_at.is_some_and(|expires_at| expires_at <= now);
    let superseded = memory.superseded_by.is_some();
    InventoryCard {
        id: memory.id,
        summary_or_excerpt,
        scope,
        contexts,
        agent_label,
        created_at: memory.created_at.to_rfc3339(),
        updated_at: updated_at.to_rfc3339(),
        tags: memory.tags,
        entities: memory.entities,
        memory_type: memory.memory_type,
        has_embedding: memory.has_embedding,
        unresolved_scope,
        expired,
        superseded,
        quality_flags,
    }
}

fn write_quality_warnings(content: &str, summary: Option<&str>, unresolved_scope: bool, tags: &[String], entity_count: usize) -> Vec<QualityWarning> {
    let mut warnings = Vec::new();
    if unresolved_scope {
        warnings.push(quality_warning("missing_scope", format!("no scope was supplied; memory was placed in {UNRESOLVED_SCOPE}")));
    }
    if summary.is_none_or(|s| s.trim().is_empty()) {
        warnings.push(quality_warning("missing_summary", "no summary supplied; recall cards will use deterministic excerpts"));
    }
    if tags.is_empty() {
        warnings.push(quality_warning("empty_tags", "no tags supplied"));
    }
    if entity_count == 0 {
        warnings.push(quality_warning("empty_entities", "no entities supplied"));
    }
    if content.len() > LARGE_CONTENT_WARNING_THRESHOLD_BYTES {
        warnings.push(quality_warning(
            "oversized_content",
            "content is large for an agent memory; consider storing a concise durable summary",
        ));
    }
    if content.contains("```")
        || content
            .lines()
            .take(20)
            .any(|line| line.trim_start().starts_with("fn ") || line.trim_start().starts_with("impl "))
    {
        warnings.push(quality_warning(
            "possible_code_dump",
            "content looks code-derived; prefer remembering durable rationale instead of source text",
        ));
    }
    warnings
}

/// Validate and normalize an optional `text_search` filter field, returning
/// the value to assign to `MemoryFilter::text_search`.
///
/// This deduplicates the `validate_optional_non_empty` + `filter.text_search = …`
/// pattern used by `admin_list`, `admin_bulk_delete`, and `admin_bulk_update`.
fn normalize_text_search(text_search: Option<String>) -> Result<Option<String>, EngineError> {
    if let Some(ts) = &text_search {
        validate_optional_non_empty("text_search", Some(ts.as_str())).map_err(EngineError::from)?;
    }
    Ok(text_search)
}

/// Replace a legacy caller spelling with the selected context's immutable
/// canonical key while preserving how the legacy value was resolved.
fn canonicalize_legacy_scope_resolution(mut resolution: ScopeResolution, selection: &ResolvedContextSelection) -> ScopeResolution {
    if let Some(primary) = selection.resolution.direct.first() {
        resolution.scope.clone_from(&primary.key);
    }
    resolution
}

/// Expand one legacy scope key into compatibility ancestors by splitting on `/`.
///
/// For example, `"org/project/conv"` becomes `["org/project/conv", "org/project", "org"]`.
/// A single-segment scope (no `/`) returns just itself.
/// An empty string returns an empty vec.
#[expect(
    clippy::string_slice,
    reason = "rfind('/') returns a byte position of an ASCII char — slicing at it cannot split a UTF-8 character"
)]
fn expand_scope_hierarchy(scope: &str) -> Vec<String> {
    if scope.is_empty() {
        return Vec::new();
    }
    let mut result = vec![scope.to_owned()];
    let mut s = scope;
    while let Some(pos) = s.rfind('/') {
        s = &s[..pos];
        if !s.is_empty() {
            result.push(s.to_owned());
        }
    }
    result
}

const MAX_LEGACY_SCOPE_DEPTH: usize = 32;
const MAX_EXPANDED_LEGACY_SCOPES: usize = 256;

/// Expand all scope keys in a list to include their ancestor scopes, deduplicating.
fn expand_scope_keys(scope_keys: &[String]) -> Result<Vec<String>, crate::error::ValidationError> {
    let mut seen = HashSet::new();
    let mut expanded = Vec::new();
    for key in scope_keys {
        let depth = key.split('/').count();
        if depth > MAX_LEGACY_SCOPE_DEPTH {
            return Err(crate::error::ValidationError::new(
                "scope",
                format!("legacy scope hierarchy depth must be at most {MAX_LEGACY_SCOPE_DEPTH}"),
            ));
        }
        for ancestor in expand_scope_hierarchy(key) {
            if !seen.insert(ancestor.clone()) {
                continue;
            }
            if expanded.len() >= MAX_EXPANDED_LEGACY_SCOPES {
                return Err(crate::error::ValidationError::new(
                    "scope",
                    format!("expanded legacy scope selection must contain at most {MAX_EXPANDED_LEGACY_SCOPES} unique values"),
                ));
            }
            expanded.push(ancestor);
        }
    }
    Ok(expanded)
}

/// Optionally expand scope hierarchy on a filter's `scopes_any`.
///
/// When `expand` is `true`, each scope key is expanded to include all ancestor
/// scopes (e.g. `"a/b/c"` also matches `"a/b"` and `"a"`). The filter is
/// mutated in place only when expansion actually adds new keys.
fn maybe_expand_scope_hierarchy(filter: &mut MemoryFilter, expand: bool) -> Result<(), crate::error::ValidationError> {
    if expand && let Some(scope_keys) = &filter.scopes_any {
        let expanded = expand_scope_keys(scope_keys)?;
        if expanded.len() != scope_keys.len() {
            filter.scopes_any = Some(expanded);
        }
    }
    Ok(())
}

/// Validated filter and context extracted from internal admin filter fields.
struct ValidatedFilter {
    filter: MemoryFilter,
    ctx: QueryContext,
}

impl TryFrom<&params::CommonFilterFields> for ValidatedFilter {
    type Error = EngineError;

    fn try_from(fields: &params::CommonFilterFields) -> Result<Self, Self::Error> {
        let principal = normalize_optional_non_empty("principal", fields.principal.clone())?;
        let agent_label = normalize_optional_non_empty("agent_label", fields.agent_label.clone())?;
        let scope = normalize_legacy_scope_value("scope", fields.scope.clone())?;
        let origin_scope = normalize_legacy_scope_value("origin_scope", fields.origin_scope.clone())?;
        let scopes_any = fields
            .scopes_any
            .clone()
            .map(|values| normalize_legacy_scope_values("scopes_any", Some(values)))
            .transpose()?;
        let tags = normalize_optional_string_array("tags", fields.tags.clone())?;
        let entity = normalize_optional_non_empty("entity", fields.entity.clone())?;
        let entity_type = normalize_optional_non_empty("entity_type", fields.entity_type.clone())?;

        Ok(Self {
            filter: MemoryFilter {
                tags,
                agent_label,
                scope,
                origin_scope,
                scopes_any,
                context_ids: fields.context_ids.clone(),
                legacy_context_ids_any: fields.legacy_context_ids_any.clone(),
                explicit_context_filter: fields.explicit_context_filter,
                memory_type: fields.memory_type,
                include_superseded: fields.include_superseded,
                entity,
                entity_type,
                ..Default::default()
            },
            ctx: QueryContext { principal },
        })
    }
}

fn normalize_bounded_legacy_values(field: &str, values: Option<Vec<String>>, max_entries: usize) -> Result<Vec<String>, crate::error::ValidationError> {
    let values = normalize_optional_string_array(field, values)?.unwrap_or_default();
    if values.len() > max_entries {
        return Err(crate::error::ValidationError::new(field, format!("accepts at most {max_entries} entries")));
    }
    if values.iter().any(|value| value.len() > MAX_CONTEXT_SURFACE_LEN) {
        return Err(crate::error::ValidationError::new(
            field,
            format!("each value accepts at most {MAX_CONTEXT_SURFACE_LEN} bytes"),
        ));
    }
    Ok(values)
}

fn normalize_legacy_scope_value(field: &str, value: Option<String>) -> Result<Option<String>, crate::error::ValidationError> {
    let value = normalize_optional_non_empty(field, value)?;
    if let Some(value) = value.as_deref() {
        validate_legacy_scope_key(value).map_err(|message| crate::error::ValidationError::new(field, message))?;
    }
    Ok(value)
}

fn normalize_legacy_scope_values(field: &str, values: Option<Vec<String>>) -> Result<Vec<String>, crate::error::ValidationError> {
    let values = normalize_bounded_legacy_values(field, values, MAX_CONTEXT_REFS)?;
    for value in &values {
        validate_legacy_scope_key(value).map_err(|message| crate::error::ValidationError::new(field, message))?;
    }
    Ok(values)
}

fn normalize_legacy_context_hints(values: Vec<String>) -> Result<Vec<String>, crate::error::ValidationError> {
    normalize_bounded_legacy_values("context_hints", Some(values), MAX_CONTEXT_HINTS)
}

/// Validate and normalize common filter fields into a `(MemoryFilter, QueryContext)` pair.
fn validate_and_normalize_filter(fields: &params::CommonFilterFields) -> Result<(MemoryFilter, QueryContext), EngineError> {
    let validated = ValidatedFilter::try_from(fields)?;
    Ok((validated.filter, validated.ctx))
}

fn reject_removed_admin_field(present: bool, removed: &str, replacement: &str) -> Result<(), EngineError> {
    if present {
        return Err(crate::error::ValidationError::new(removed, format!("removed from admin API; use {replacement}")).into());
    }
    Ok(())
}

impl TryFrom<params::EntityInput> for crate::types::Entity {
    type Error = crate::error::ValidationError;

    fn try_from(entity: params::EntityInput) -> Result<Self, Self::Error> {
        let (name, entity_type) = crate::validation::normalize_entity_parts(&entity.name, &entity.entity_type)?;
        Ok(Self { name, entity_type })
    }
}

fn normalize_entity_inputs(entities: Vec<params::EntityInputItem>) -> Result<Vec<crate::types::Entity>, crate::error::ValidationError> {
    entities.into_iter().map(params::EntityInput::from).map(TryInto::try_into).collect()
}

fn normalize_optional_entity_inputs(entities: Option<Vec<params::EntityInputItem>>) -> Result<Option<Vec<crate::types::Entity>>, crate::error::ValidationError> {
    entities.map(normalize_entity_inputs).transpose()
}

fn normalize_optional_access_policy(policy: Option<params::AccessPolicyInput>) -> Option<crate::types::AccessPolicy> {
    policy.map(Into::into)
}

#[expect(clippy::multiple_inherent_impl, reason = "tool router macro methods are kept separate from constructors and helpers")]
#[tool_router]
impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> LocalHoldServer<S> {
    #[tool(
        description = "Resolve governed context IDs, keys, typed identities, aliases, natural-language queries, and weak hints. An empty query returns a paginated authorized context catalog."
    )]
    async fn context_resolve(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<ContextResolveParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let principal = request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL);
        let query = params.query.as_deref().map(str::trim).filter(|query| !query.is_empty());
        if query.is_none() && params.context.refs.is_empty() && params.context.hints.is_empty() {
            let limit = params.limit.unwrap_or(20).clamp(1, 500);
            let records = self
                .engine
                .store()
                .list_context_records(principal, false, params.offset, limit)
                .await
                .map_err(EngineError::from)?;
            let catalog = records.iter().map(|record| ContextDescriptor::from(&record.context)).collect::<Vec<_>>();
            let next_offset = (catalog.len() == limit).then_some(params.offset.saturating_add(catalog.len()));
            let policy_state = self.context_policy_state(principal).await?;
            let policies = policy_state
                .kinds
                .iter()
                .map(|definition| Self::effective_policy_for(&policy_state, &[], &definition.kind, &[]))
                .collect::<Vec<_>>();
            return success_json(&ContextResolveResponse {
                resolution: ContextResolution {
                    policy_guidance: Self::policy_guidance_for(policies),
                    broad_search: true,
                    ..ContextResolution::default()
                },
                catalog,
                next_offset,
            });
        }
        match self.resolve_context_selection(principal, Some(&params.context), query, false).await {
            Ok(selection) => success_json(&ContextResolveResponse {
                resolution: selection.resolution,
                catalog: Vec::new(),
                next_offset: None,
            }),
            Err(error) => Ok(error),
        }
    }

    #[tool(
        description = "Create a private governed context when effective policy permits it. Exact key, alias, or identity matches are always reused; fuzzy duplicate candidates require confirm_distinct_from."
    )]
    async fn context_create(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<ContextCreateParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let key = params.key.trim().to_owned();
        let display_name = params.display_name.trim().to_owned();
        if key.is_empty()
            || display_name.is_empty()
            || key.len() > MAX_CONTEXT_SURFACE_LEN
            || display_name.len() > MAX_CONTEXT_DISPLAY_NAME_LEN
            || params.description.as_ref().is_some_and(|value| value.len() > MAX_CONTEXT_DESCRIPTION_LEN)
            || params.confirm_distinct_from.len() > MAX_CONTEXT_CONFIRMATIONS
        {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context_create"),
                "context creation fields are blank or exceed their documented limits",
                Some("Use a bounded stable key, display name, description, and at most five confirmation IDs."),
                false,
            ));
        }
        let mut unique_confirmations = params.confirm_distinct_from.clone();
        unique_confirmations.sort_unstable();
        unique_confirmations.dedup();
        if unique_confirmations.len() != params.confirm_distinct_from.len() {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("confirm_distinct_from"),
                "confirmation IDs must be unique",
                Some("Submit each current candidate ID exactly once."),
                false,
            ));
        }
        let identity = match params.identity.as_ref().map(normalize_context_identity).transpose() {
            Ok(identity) => identity,
            Err(message) => {
                return Ok(tool_error(
                    ToolErrorCode::InvalidParams,
                    Some("identity"),
                    message,
                    Some("Use git_remote, an approved absolute uri, or a namespaced_id; put local paths in resolver hints."),
                    false,
                ));
            }
        };
        let policy_state = self.context_policy_state(&principal).await?;
        let normalized_key = normalize_context_key(&key);
        let mut exact = self
            .engine
            .store()
            .find_context_records(&principal, true, &ContextExactLookup::Key {
                kind: Some(params.kind.clone()),
                normalized_key: normalized_key.clone(),
            })
            .await
            .map_err(EngineError::from)?;
        if let Some(identity) = &identity {
            for record in self
                .engine
                .store()
                .find_context_records(&principal, true, &ContextExactLookup::Identity {
                    kind: params.kind.clone(),
                    identity: identity.clone(),
                })
                .await
                .map_err(EngineError::from)?
            {
                if !exact.iter().any(|candidate| candidate.context.id == record.context.id) {
                    exact.push(record);
                }
            }
        }
        match exact.as_slice() {
            [record] => {
                if record.context.lifecycle == crate::context::ContextLifecycle::Archived {
                    return Ok(tool_error_with_details(
                        ToolErrorCode::Conflict,
                        Some("key"),
                        "exact context key, alias, or identity belongs to an archived context",
                        Some("Reactivate the reserved context in the TUI; agent tools cannot replace archived identities."),
                        false,
                        Some(serde_json::json!({
                            "context": ContextDescriptor::from(&record.context),
                            "lifecycle": record.context.lifecycle
                        })),
                        Vec::new(),
                    ));
                }
                let policy = Self::effective_policy_for(&policy_state, &[], &record.context.kind, &[]);
                return success_json(&ContextCreateResponse {
                    context: ContextDescriptor::from(&record.context),
                    created: false,
                    identity: None,
                    policy_guidance: Self::policy_guidance_for([policy]),
                });
            }
            [_, _, ..] => {
                let candidates = exact
                    .iter()
                    .map(|record| ContextCandidate {
                        context: ContextDescriptor::from(&record.context),
                        score: 1.0,
                        matched_by: vec!["exact_key_or_identity".to_owned()],
                    })
                    .collect();
                return Ok(Self::context_resolution_error(
                    ToolErrorCode::ContextAmbiguous,
                    "key",
                    "exact context key, alias, or identity matched multiple authorized contexts",
                    candidates,
                ));
            }
            [] => {}
        }
        let parent_id = if let Some(parent) = params.parent.as_ref() {
            let envelope = ContextEnvelope {
                refs: vec![parent.clone()],
                ..ContextEnvelope::default()
            };
            match self.resolve_context_selection(&principal, Some(&envelope), None, false).await {
                Ok(selection) => selection.direct_ids.first().copied(),
                Err(error) => return Ok(error),
            }
        } else {
            None
        };
        let parent_effective = if let Some(parent_id) = parent_id {
            match self.engine.store().expand_context_selection(&[parent_id], &principal, false).await {
                Ok(contexts) => contexts,
                Err(error) => return Ok(Self::context_expansion_error("parent", error)),
            }
        } else {
            Vec::new()
        };
        let parent_effective_ids = parent_effective.iter().map(|context| context.id).collect::<Vec<_>>();
        let parent_policy_records = if parent_effective_ids.is_empty() {
            Vec::new()
        } else {
            parent_effective
                .into_iter()
                .map(|context| ContextRecord {
                    context,
                    aliases: Vec::new(),
                    identities: Vec::new(),
                    hints: Vec::new(),
                })
                .collect()
        };
        let effective_policy = Self::effective_policy_for(&policy_state, &parent_policy_records, &params.kind, &parent_effective_ids);
        let policy_guidance = Self::policy_guidance_for([effective_policy.clone()]);
        if !effective_policy.ambiguities.is_empty() {
            return Ok(tool_error_with_details(
                ToolErrorCode::ContextAmbiguous,
                Some("kind"),
                "effective context creation policy is ambiguous",
                Some("Resolve the active anchor-policy conflict in the TUI before retrying."),
                false,
                Some(serde_json::json!({
                    "policy_guidance": policy_guidance,
                    "ambiguities": effective_policy.ambiguities
                })),
                Vec::new(),
            ));
        }
        if !effective_policy.allowed || !effective_policy.agent_creation {
            return Ok(tool_error_with_details(
                ToolErrorCode::AccessDenied,
                Some("kind"),
                "effective policy does not allow agent creation for this context kind",
                Some("Use the TUI to enable agent creation within operator ceilings."),
                false,
                Some(serde_json::json!({ "policy_guidance": policy_guidance })),
                Vec::new(),
            ));
        }
        if effective_policy.require_identity && identity.is_none() {
            return Ok(tool_error_with_details(
                ToolErrorCode::ContextRequired,
                Some("identity"),
                "effective policy requires a durable typed identity for this context kind",
                Some("Supply git_remote, an approved absolute uri, or a namespaced_id."),
                false,
                Some(serde_json::json!({ "policy_guidance": policy_guidance })),
                Vec::new(),
            ));
        }
        if let Some(identity) = &identity
            && !effective_policy.allowed_identity_schemes.contains(&identity.scheme)
        {
            return Ok(tool_error_with_details(
                ToolErrorCode::AccessDenied,
                Some("identity.scheme"),
                "effective policy does not allow this identity scheme for the context kind",
                Some("Use one of the identity schemes listed by effective policy."),
                false,
                Some(serde_json::json!({
                    "allowed_identity_schemes": effective_policy.allowed_identity_schemes,
                    "policy_guidance": policy_guidance
                })),
                Vec::new(),
            ));
        }
        let active_records = self.authorized_context_records(&principal).await?;
        let late_exact = active_records
            .iter()
            .filter(|record| {
                record.context.kind == params.kind
                    && (normalize_context_key(&record.context.key) == normalized_key
                        || record.aliases.iter().any(|alias| normalize_context_key(alias) == normalized_key)
                        || identity.as_ref().is_some_and(|identity| record.identities.contains(identity)))
            })
            .collect::<Vec<_>>();
        match late_exact.as_slice() {
            [record] => {
                let policy = Self::effective_policy_for(&policy_state, &[], &record.context.kind, &[]);
                return success_json(&ContextCreateResponse {
                    context: ContextDescriptor::from(&record.context),
                    created: false,
                    identity: None,
                    policy_guidance: Self::policy_guidance_for([policy]),
                });
            }
            [_, _, ..] => {
                let candidates = late_exact
                    .iter()
                    .map(|record| ContextCandidate {
                        context: ContextDescriptor::from(&record.context),
                        score: 1.0,
                        matched_by: vec!["exact_key_or_identity".to_owned()],
                    })
                    .collect();
                return Ok(Self::context_resolution_error(
                    ToolErrorCode::ContextAmbiguous,
                    "key",
                    "exact context key, alias, or identity matched multiple authorized contexts",
                    candidates,
                ));
            }
            [] => {}
        }
        let candidates = fuzzy_context_candidates(&active_records, &format!("{key} {display_name}"), Some(&params.kind));
        let mut candidate_ids = candidates.iter().map(|candidate| candidate.context.id).collect::<Vec<_>>();
        candidate_ids.sort_unstable();
        let mut confirmed = params.confirm_distinct_from.clone();
        confirmed.sort_unstable();
        confirmed.dedup();
        if !candidate_ids.is_empty() && candidate_ids != confirmed {
            return Ok(tool_error_with_details(
                ToolErrorCode::ContextAmbiguous,
                Some("confirm_distinct_from"),
                "similar authorized contexts require explicit distinctness confirmation",
                Some("Review the candidates and repeat context_create with every current candidate ID in confirm_distinct_from."),
                false,
                Some(serde_json::json!({
                    "candidates": candidates,
                    "policy_guidance": policy_guidance,
                    "retry": {
                        "confirm_distinct_from": candidate_ids
                    },
                    "note": "Repeat the original request locally with this confirmation set; raw identity and parent locators are intentionally omitted."
                })),
                Vec::new(),
            ));
        }
        let id = ContextId::new();
        let draft = ContextCreateDraft {
            id,
            kind: params.kind,
            key,
            normalized_key,
            display_name,
            description: trim_optional_text(params.description),
            owner_principal: principal.clone(),
            guidance: None,
            parent_id,
            aliases: Vec::new(),
            identities: identity.clone().into_iter().collect(),
            resolver_hints: Vec::new(),
            confirm_distinct_from: params.confirm_distinct_from,
            enforce_fuzzy_confirmation: true,
            frozen: false,
        };
        let audit = ContextAuditDraft {
            actor_principal: principal,
            action: "context_created".to_owned(),
            context_id: Some(id),
            memory_id: None,
            details: Some(serde_json::json!({
                "kind": draft.kind,
                "key": draft.key,
                "identity_scheme": identity.as_ref().map(|value| value.scheme.as_str())
            })),
        };
        let created = match self.engine.store().create_context(&draft, &audit).await {
            Ok(created) => created,
            Err(crate::error::StoreError::Conflict(message)) if message.starts_with("fuzzy context candidates changed") => {
                let Ok(current_records) = self.authorized_context_records(&draft.owner_principal).await else {
                    return Ok(tool_error(
                        ToolErrorCode::Internal,
                        Some("confirm_distinct_from"),
                        "context candidates changed and could not be refreshed",
                        Some("Call context_resolve after checking backend health, then retry."),
                        true,
                    ));
                };
                let current_candidates = fuzzy_context_candidates(&current_records, &format!("{} {}", draft.key, draft.display_name), Some(&draft.kind));
                let mut current_ids = current_candidates.iter().map(|candidate| candidate.context.id).collect::<Vec<_>>();
                current_ids.sort_unstable();
                return Ok(tool_error_with_details(
                    ToolErrorCode::ContextAmbiguous,
                    Some("confirm_distinct_from"),
                    message,
                    Some("Resolve the current candidates and retry with a fresh confirmation set."),
                    true,
                    Some(serde_json::json!({
                        "candidates": current_candidates,
                        "policy_guidance": policy_guidance,
                        "retry": {
                            "confirm_distinct_from": current_ids
                        }
                    })),
                    Vec::new(),
                ));
            }
            Err(crate::error::StoreError::Conflict(message)) => {
                let mut winners = self
                    .engine
                    .store()
                    .find_context_records(&draft.owner_principal, true, &ContextExactLookup::Key {
                        kind: Some(draft.kind.clone()),
                        normalized_key: draft.normalized_key.clone(),
                    })
                    .await
                    .map_err(EngineError::from)?;
                if let Some(identity) = identity.as_ref() {
                    for record in self
                        .engine
                        .store()
                        .find_context_records(&draft.owner_principal, true, &ContextExactLookup::Identity {
                            kind: draft.kind.clone(),
                            identity: identity.clone(),
                        })
                        .await
                        .map_err(EngineError::from)?
                    {
                        if !winners.iter().any(|candidate| candidate.context.id == record.context.id) {
                            winners.push(record);
                        }
                    }
                }
                if let [winner] = winners.as_slice()
                    && winner.context.lifecycle == crate::context::ContextLifecycle::Active
                {
                    return success_json(&ContextCreateResponse {
                        context: ContextDescriptor::from(&winner.context),
                        created: false,
                        identity: None,
                        policy_guidance,
                    });
                }
                return Ok(tool_error_with_details(
                    ToolErrorCode::Conflict,
                    Some("key"),
                    "context creation conflicted with a concurrent exact definition",
                    Some("Resolve the exact key, alias, or identity and retry using the returned context."),
                    true,
                    Some(serde_json::json!({
                        "candidates": winners.iter().map(|record| ContextDescriptor::from(&record.context)).collect::<Vec<_>>(),
                        "reason": message
                    })),
                    Vec::new(),
                ));
            }
            Err(error) => return Err(EngineError::from(error).into()),
        };
        success_json(&ContextCreateResponse {
            context: ContextDescriptor::from(&created),
            created: true,
            identity: identity.as_ref().map(params::ContextIdentityDescriptor::from),
            policy_guidance,
        })
    }

    #[tool(
        description = "Remember durable information. Governed writes require a context selection, a safe policy default, or explicit context.allow_unresolved=true. Legacy scope/context_hints remain compatibility adapters; entities/access_policy accept shorthand or full objects."
    )]
    async fn remember(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<RememberParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let prepared = match self.prepare_remember(params, principal.clone(), self.engine.now()).await {
            Ok(prepared) => prepared,
            Err(error) => return error.into_tool_result(None),
        };
        let PreparedRemember {
            memory,
            supersedes,
            metadata,
            scope_resolution,
            context_resolution,
            direct_context_ids,
            duplicate_candidates,
            warnings,
            created_legacy_context,
        } = prepared;
        let scope = scope_resolution.scope.clone();
        let unresolved_scope = scope_resolution.unresolved_scope;
        let id = match self
            .engine
            .store_memory_with_metadata_contexts(memory, supersedes.as_ref(), &metadata, &direct_context_ids)
            .await
        {
            Ok(id) => id,
            Err(error) => {
                if let Some(context_id) = created_legacy_context {
                    self.rollback_created_legacy_contexts(&mut vec![context_id], &principal).await?;
                }
                return Err(error.into());
            }
        };
        let next_action = next_action_for_warnings(&warnings);
        success_json(&RememberResponse {
            operation: operation_summary(OperationStatus::Applied, 1, warnings.clone(), next_action),
            id,
            scope,
            unresolved_scope,
            scope_resolution,
            context_resolution: Some(context_resolution.clone()),
            contexts: context_resolution.direct,
            duplicate_candidates,
            warnings,
        })
    }

    #[tool(
        description = "Remember multiple durable memories atomically using the server-resolved principal. memories items may be string content shorthand or full remember objects; caps at max_batch_size; returns per-item scope, duplicate, and warning details."
    )]
    async fn remember_many(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<RememberManyParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        if let Some(error) = batch_len_tool_error(
            "memories",
            params.memories.len(),
            self.engine.limits().max_batch_size,
            "Split the remember_many request into smaller batches.",
        ) {
            return Ok(error);
        }

        let now = self.engine.now();
        let params = params.memories.into_iter().map(RememberParams::from).collect::<Vec<_>>();
        for (index, item) in params.iter().enumerate() {
            if let Err(error) = self.prevalidate_remember(item, &principal, now) {
                return error.into_tool_result(Some(index));
            }
        }
        let item_count = params.len();
        let mut memories = Vec::with_capacity(item_count);
        let mut supersedes_list = Vec::with_capacity(item_count);
        let mut metadata = Vec::with_capacity(item_count);
        let mut direct_context_ids = Vec::with_capacity(item_count);
        let mut item_responses = Vec::with_capacity(item_count);
        let mut all_warnings = Vec::new();
        let mut created_legacy_contexts = Vec::new();

        for (index, params) in params.into_iter().enumerate() {
            let prepared = match self.prepare_remember(params, principal.clone(), now).await {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.rollback_created_legacy_contexts(&mut created_legacy_contexts, &principal).await?;
                    return error.into_tool_result(Some(index));
                }
            };
            let PreparedRemember {
                memory,
                supersedes,
                metadata: metadata_item,
                scope_resolution,
                context_resolution,
                direct_context_ids: direct_ids,
                duplicate_candidates,
                warnings,
                created_legacy_context,
            } = prepared;
            created_legacy_contexts.extend(created_legacy_context);
            let scope = scope_resolution.scope.clone();
            let unresolved_scope = scope_resolution.unresolved_scope;
            memories.push(memory);
            supersedes_list.push(supersedes);
            metadata.push(metadata_item);
            direct_context_ids.push(direct_ids);
            all_warnings.extend(warnings.iter().cloned());
            item_responses.push(RememberManyItemResponse {
                id: MemoryId::new(),
                scope,
                unresolved_scope,
                scope_resolution,
                context_resolution: Some(context_resolution.clone()),
                contexts: context_resolution.direct,
                duplicate_candidates,
                warnings,
            });
        }

        let ids = match self
            .engine
            .batch_store_with_metadata_contexts(memories, supersedes_list, metadata, direct_context_ids)
            .await
        {
            Ok(ids) => ids,
            Err(error) => {
                self.rollback_created_legacy_contexts(&mut created_legacy_contexts, &principal).await?;
                return Err(error.into());
            }
        };
        for (id, response) in ids.iter().copied().zip(item_responses.iter_mut()) {
            response.id = id;
        }

        let next_action = next_action_for_warnings(&all_warnings);
        let changed = u64::try_from(ids.len()).map_err(|_err| rmcp::ErrorData::internal_error("remember_many changed count overflowed u64".to_owned(), None))?;
        success_json(&RememberManyResponse {
            operation: operation_summary(OperationStatus::Applied, changed, all_warnings, next_action),
            memories: item_responses,
        })
    }

    #[tool(
        description = "Recall compact memory cards and record lightweight search impressions. Scope may be a key, alias, matcher-containing value, or context_hints. Full content is omitted; call read/read_many for IDs."
    )]
    async fn recall(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<RecallParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let mut filter = MemoryFilter {
            tags: normalize_optional_string_array("tags", params.tags).map_err(EngineError::from)?,
            entity: normalize_optional_non_empty("entity", params.entity).map_err(EngineError::from)?,
            ..Default::default()
        };
        let context_hints = normalize_legacy_context_hints(params.context_hints).map_err(EngineError::from)?;
        if params.context.is_some() && (params.scope.is_some() || !context_hints.is_empty()) {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context"),
                "context cannot be combined with legacy scope or context_hints",
                Some("Use the shared context envelope alone, or use only legacy scope/context_hints."),
                false,
            ));
        }
        let mut warnings = Vec::new();
        let (scope_resolution, context_selection) = if let Some(envelope) = params.context.as_ref() {
            let selection = match self
                .resolve_context_selection(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), Some(envelope), None, false)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            (None, selection)
        } else if params.scope.is_some() || !context_hints.is_empty() {
            let resolution = self
                .resolve_scope(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), params.scope, &context_hints)
                .await?;
            let selection = match self
                .resolve_legacy_context_selection(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), &resolution, false)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            let resolution = canonicalize_legacy_scope_resolution(resolution, &selection);
            if resolution.unresolved_scope {
                warnings.push(quality_warning(
                    "unresolved_scope",
                    "legacy context hints did not resolve; broad authorized search was applied",
                ));
            }
            warnings.push(quality_warning("legacy_scope_adapter", "legacy scope filtering was adapted to governed context membership"));
            (Some(resolution), selection)
        } else {
            let selection = match self
                .resolve_context_selection(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), None, None, false)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            (None, selection)
        };
        filter.context_ids = Some(context_selection.effective_ids.clone());
        filter.explicit_context_filter = !context_selection.effective_ids.is_empty();
        let outcome = self
            .engine
            .search_memories(SearchRequest {
                query: params.query,
                limit: params.limit,
                filter,
                ctx: Self::caller_context_for(request_principal.as_deref()),
                max_distance: None,
                keywords: normalize_optional_non_empty("literal_terms", params.literal_terms).map_err(EngineError::from)?,
                search_mode: params.search_mode,
                context: normalize_optional_non_empty("query_context", params.query_context).map_err(EngineError::from)?,
            })
            .await?;
        let search_mode = outcome.search_mode;
        let mut weak_result_count = 0_usize;
        let mut results = Vec::new();
        let reranker_blend_weight = self.engine.search_config().reranker.blend_weight;
        let views = self
            .memory_views(outcome.results.iter().map(|result| result.memory.clone()).collect(), request_principal.as_deref())
            .await?;
        for (result, view) in outcome.results.iter().zip(views) {
            let card = Self::recall_card_from_memory_view(result, view, reranker_blend_weight);
            if card.r#match.quality == MatchQuality::Weak && !params.include_weak {
                weak_result_count = weak_result_count.saturating_add(1);
            } else {
                results.push(card);
            }
        }
        success_json(&RecallResponse {
            search_mode,
            count: results.len(),
            weak_result_count,
            scope_resolution,
            context_resolution: context_selection.resolution,
            warnings,
            results,
        })
    }

    #[tool(
        description = "Read one full memory by id. Trusted principals record a meaningful read activity event only for full-access reads; redacted and anonymous public reads do not."
    )]
    async fn read(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<ReadParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let id = params.id;
        let memory = self.engine.get_memory(&id, request_principal.as_deref()).await?;
        let Some(mem) = memory else {
            return Ok(tool_error(
                ToolErrorCode::NotFound,
                Some("id"),
                format!("memory not found: {id}"),
                Some("Check the memory ID or use recall to find a visible memory."),
                false,
            ));
        };
        let use_outcome = if let Some(principal) = request_principal.as_deref() {
            self.engine.record_memory_use(vec![id], principal, READ_EVENT_WEIGHT).await.unwrap_or_default()
        } else {
            RecordUseOutcome::default()
        };
        let view = self.memory_view(mem, request_principal.as_deref()).await?;
        let item = Self::full_read_item_from_memory_view(id, view, use_outcome.recorded > 0);
        let Some(memory) = item.memory else {
            return Err(rmcp::ErrorData::internal_error("found read item omitted memory".to_owned(), None));
        };
        success_json(&ReadResponse {
            operation: operation_summary(OperationStatus::NoOp, 0, Vec::new(), NextAction::None),
            memory,
            summary: item.summary,
            scope: item.scope,
            contexts: item.contexts,
            agent_label: item.agent_label,
            created_by_principal: item.created_by_principal,
            quality_flags: item.quality_flags,
            unresolved_scope: item.unresolved_scope,
            activity_recorded: item.activity_recorded,
        })
    }

    #[tool(
        description = "Read multiple full memories by id. Preserves input order, returns per-item not_found for missing/unreadable IDs, caps at max_batch_size, and records one read activity event only for full-access items read by trusted principals."
    )]
    async fn read_many(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<ReadManyParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        if let Some(error) = batch_len_tool_error(
            "ids",
            params.ids.len(),
            self.engine.limits().max_batch_size,
            "Pass one or more memory IDs, and split large read_many requests into smaller batches.",
        ) {
            return Ok(error);
        }

        let requested_ids = params.ids;
        let found_by_id = self.engine.get_memories(&requested_ids, request_principal.as_deref()).await?;
        let mut found = Vec::new();
        let mut items = Vec::with_capacity(requested_ids.len());
        for (index, id) in requested_ids.into_iter().enumerate() {
            match found_by_id.get(&id).cloned() {
                Some(mem) => {
                    found.push((index, id, mem));
                }
                None => items.push((index, ReadManyItemResponse {
                    id,
                    status: ReadManyStatus::NotFound,
                    memory: None,
                    summary: None,
                    scope: None,
                    contexts: Vec::new(),
                    agent_label: None,
                    created_by_principal: None,
                    quality_flags: Vec::new(),
                    unresolved_scope: false,
                    activity_recorded: false,
                })),
            }
        }

        let found_ids = found.iter().map(|(_index, id, _mem)| *id).collect::<Vec<_>>();
        let activity_recorded_ids = if let Some(principal) = request_principal.as_deref() {
            self.engine
                .record_memory_use(found_ids.clone(), principal, READ_EVENT_WEIGHT)
                .await
                .unwrap_or_default()
                .recorded_ids
                .into_iter()
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let views = self
            .memory_views(found.iter().map(|(_index, _id, memory)| memory.clone()).collect(), request_principal.as_deref())
            .await?;
        for ((index, id, _memory), view) in found.into_iter().zip(views) {
            let activity_recorded = activity_recorded_ids.contains(&id);
            items.push((index, Self::full_read_item_from_memory_view(id, view, activity_recorded)));
        }
        items.sort_by_key(|(index, _item)| *index);
        let results = items.into_iter().map(|(_index, item)| item).collect();
        success_json(&ReadManyResponse {
            operation: operation_summary(OperationStatus::NoOp, 0, Vec::new(), NextAction::None),
            results,
        })
    }

    #[tool(
        description = "Revise an existing memory using the server-resolved principal for write authorization. Scope accepts key/alias/matcher/context_hints; entities and access_policy accept shorthand or full objects."
    )]
    async fn revise(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<ReviseParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let id = params.id;
        let summary = trim_optional_text(params.summary);
        let agent_label = trim_optional_text(params.agent_label);
        let context_hints = normalize_legacy_context_hints(params.context_hints).map_err(EngineError::from)?;
        let tags = normalize_optional_string_array("tags", params.tags).map_err(EngineError::from)?;
        let entities = normalize_optional_entity_inputs(params.entities).map_err(EngineError::from)?;
        if params.context.is_some() && (params.scope.is_some() || !context_hints.is_empty()) {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context"),
                "context cannot be combined with legacy scope or context_hints",
                Some("Use the shared context envelope alone, or use only legacy scope/context_hints."),
                false,
            ));
        }
        let mut scope_resolution = None;
        let mut context_resolution = None;
        let mut replacement_context_ids = None;
        let mut created_legacy_context = None;
        let mut legacy_expected_revision = None;
        let mut operation_warnings = Vec::new();
        let scope_update = if let Some(envelope) = params.context.as_ref() {
            let selection = match self.resolve_context_selection(&principal, Some(envelope), None, true).await {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            let scope = selection
                .resolution
                .direct
                .first()
                .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), |context| context.key.clone());
            replacement_context_ids = Some(selection.direct_ids);
            context_resolution = Some(selection.resolution);
            if replacement_context_ids.as_ref().is_some_and(Vec::is_empty) {
                operation_warnings.push(quality_warning(
                    "missing_scope",
                    format!("context classification was explicitly deferred; memory was placed in {UNRESOLVED_SCOPE}"),
                ));
            }
            Some(scope)
        } else if params.scope.is_some() || !context_hints.is_empty() {
            let resolution = self.resolve_scope(&principal, params.scope, &context_hints).await?;
            let selection = match self.resolve_legacy_context_selection(&principal, &resolution, true).await {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            let resolution = canonicalize_legacy_scope_resolution(resolution, &selection);
            created_legacy_context = selection.created_legacy_context;
            let memory = match self.engine.get_memory(&id, Some(&principal)).await {
                Ok(memory) => memory,
                Err(error) => {
                    self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                    return Err(error.into());
                }
            };
            let Some(memory) = memory else {
                self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                return Ok(tool_error(
                    ToolErrorCode::NotFound,
                    Some("id"),
                    format!("memory not found: {id}"),
                    Some("Check the memory ID or use recall to find a visible memory."),
                    false,
                ));
            };
            let Some(expected_revision) = memory.optimistic_revision() else {
                self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                return Err(EngineError::from(crate::error::StoreError::Conflict(format!("memory {id} has no optimistic revision"))).into());
            };
            legacy_expected_revision = Some(expected_revision);
            let existing = match self.engine.store().get_memory_contexts(&id, &principal).await {
                Ok(existing) => existing,
                Err(error) => {
                    self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                    return Err(EngineError::from(error).into());
                }
            };
            let stored_membership_count = match self.engine.store().count_memory_contexts_for_write(&id, &principal).await {
                Ok(count) => count,
                Err(error) => {
                    self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                    return Err(EngineError::from(error).into());
                }
            };
            if stored_membership_count != Some(existing.len()) {
                self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                return Ok(tool_error(
                    ToolErrorCode::Conflict,
                    Some("scope"),
                    "legacy scope cannot safely preserve every existing context membership",
                    Some("Restore visibility to every attached context, or use an explicit context envelope after confirming the complete replacement set."),
                    false,
                ));
            }
            let Some(selected_id) = selection.direct_ids.first().copied() else {
                self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                return Ok(tool_error(
                    ToolErrorCode::ContextRequired,
                    Some("scope"),
                    "legacy scope did not resolve to a direct context",
                    Some("Use context_resolve, then retry revise with an explicit context reference."),
                    false,
                ));
            };
            let mut contexts = vec![selected_id];
            contexts.extend(
                existing
                    .into_iter()
                    .filter(|membership| membership.ordinal != 0)
                    .map(|membership| membership.context.id)
                    .filter(|context_id| *context_id != selected_id),
            );
            replacement_context_ids = Some(contexts);
            context_resolution = Some(selection.resolution);
            operation_warnings.push(quality_warning(
                "legacy_scope_adapter",
                "legacy scope replaced only the compatibility-primary context; other memberships were preserved",
            ));
            let scope = Some(resolution.scope.clone());
            scope_resolution = Some(resolution);
            scope
        } else {
            None
        };
        let metadata_patch = MetadataPatch {
            scope_key: scope_update.clone(),
            summary,
            clear_summary: false,
            agent_label,
            clear_agent_label: false,
        };
        let metadata_patch = (!metadata_patch.is_empty()).then_some(metadata_patch);
        let update = MemoryUpdate {
            content: params.content,
            tags,
            access_policy: normalize_optional_access_policy(params.access_policy),
            importance: params.importance.map(crate::types::Importance::new),
            expires_at: None,
            confidence: params.confidence.map(crate::types::Confidence::new),
            source_conversation: scope_update.clone(),
            entities,
        };
        let update_result = if let Some(expected_revision) = legacy_expected_revision {
            self.engine
                .update_memory_if_unmodified_with_metadata_contexts(id, expected_revision, update, metadata_patch, replacement_context_ids, &principal)
                .await
        } else {
            self.engine
                .update_memory_with_metadata_contexts(id, update, metadata_patch, replacement_context_ids, &principal)
                .await
        };
        let update_outcome = match update_result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
                return Err(error.into());
            }
        };
        if update_outcome.outcome != WriteOutcome::Applied {
            self.rollback_created_legacy_context(created_legacy_context, &principal).await?;
        }
        match update_outcome.outcome {
            WriteOutcome::NotFound => Ok(tool_error(
                ToolErrorCode::NotFound,
                Some("id"),
                format!("memory not found: {id}"),
                Some("Check the memory ID or use recall to find a visible memory."),
                false,
            )),
            WriteOutcome::Denied => Ok(tool_error(
                ToolErrorCode::AccessDenied,
                Some("id"),
                format!("access denied: principal cannot modify memory {id}"),
                Some("Use a trusted principal that owns or is allowed to modify this memory."),
                false,
            )),
            WriteOutcome::Applied => {
                let next_action = next_action_for_warnings(&operation_warnings);
                let contexts = self
                    .engine
                    .store()
                    .get_memory_contexts(&id, &principal)
                    .await
                    .map_err(EngineError::from)?
                    .iter()
                    .map(|membership| ContextDescriptor::from(&membership.context))
                    .collect();
                success_json(&UpdateResponse {
                    operation: operation_summary(OperationStatus::Applied, 1, operation_warnings, next_action),
                    updated: true,
                    scope_resolution,
                    context_resolution,
                    contexts,
                })
            }
        }
    }

    #[tool(description = "Forget a memory by id using the server-resolved principal for write authorization; destructive delete when authorized.")]
    async fn forget(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<ForgetParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let id = params.id;
        let outcome = self.engine.delete_memory(&id, &principal).await?;
        match outcome {
            WriteOutcome::NotFound => Ok(tool_error(
                ToolErrorCode::NotFound,
                Some("id"),
                format!("memory not found: {id}"),
                Some("Check the memory ID or use recall to find a visible memory."),
                false,
            )),
            WriteOutcome::Denied => Ok(tool_error(
                ToolErrorCode::AccessDenied,
                Some("id"),
                format!("access denied: principal cannot delete memory {id}"),
                Some("Use a trusted principal that owns or is allowed to delete this memory."),
                false,
            )),
            WriteOutcome::Applied => success_json(&DeleteResponse {
                operation: operation_summary(OperationStatus::Applied, 1, Vec::new(), NextAction::None),
                deleted: true,
            }),
        }
    }

    #[tool(
        description = "Return deterministic structured context grouped into relevant memories, decisions, WIP, lessons, stale candidates, suggested reads, and recommended actions. Scope accepts key/alias/matcher/context_hints."
    )]
    async fn brief(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<BriefParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let supplied_query = params.query.as_deref().map(str::trim).filter(|query| !query.is_empty()).map(ToOwned::to_owned);
        let query = params.query.unwrap_or_else(|| "project memory decisions wip lessons".to_owned());
        let limit = params.limit;
        let context_hints = normalize_legacy_context_hints(params.context_hints).map_err(EngineError::from)?;
        if params.context.is_some() && (params.scope.is_some() || !context_hints.is_empty()) {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context"),
                "context cannot be combined with legacy scope or context_hints",
                Some("Use the shared context envelope alone, or use only legacy scope/context_hints."),
                false,
            ));
        }
        let mut filter = MemoryFilter::default();
        let scope_requested = params.scope.is_some() || !context_hints.is_empty();
        let mut warnings = Vec::new();
        let (scope_resolution, context_selection) = if let Some(envelope) = params.context.as_ref() {
            let selection = match self
                .resolve_context_selection(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), Some(envelope), None, false)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            (None, selection)
        } else if scope_requested {
            let resolution = self
                .resolve_scope(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), params.scope, &context_hints)
                .await?;
            let selection = match self
                .resolve_legacy_context_selection(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), &resolution, false)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            let resolution = canonicalize_legacy_scope_resolution(resolution, &selection);
            if resolution.unresolved_scope {
                warnings.push(quality_warning(
                    "unresolved_scope",
                    "legacy context hints did not resolve; broad authorized search was applied",
                ));
            }
            warnings.push(quality_warning("legacy_scope_adapter", "legacy scope filtering was adapted to governed context membership"));
            (Some(resolution), selection)
        } else {
            let selection = match self
                .resolve_context_selection(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL), None, None, false)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            (None, selection)
        };
        filter.context_ids = Some(context_selection.effective_ids.clone());
        filter.explicit_context_filter = !context_selection.effective_ids.is_empty();
        let outcome = self
            .engine
            .search_memories(SearchRequest {
                query,
                limit,
                filter,
                ctx: Self::caller_context_for(request_principal.as_deref()),
                max_distance: None,
                keywords: None,
                search_mode: None,
                context: None,
            })
            .await?;
        let mut relevant = Vec::new();
        let mut decisions = Vec::new();
        let mut wip = Vec::new();
        let mut lessons = Vec::new();
        let mut stale_candidates = Vec::new();
        let reranker_blend_weight = self.engine.search_config().reranker.blend_weight;
        let views = self
            .memory_views(outcome.results.iter().map(|result| result.memory.clone()).collect(), request_principal.as_deref())
            .await?;
        for (result, view) in outcome.results.iter().zip(views) {
            let card = Self::recall_card_from_memory_view(result, view, reranker_blend_weight);
            if card.r#match.quality == MatchQuality::Weak {
                stale_candidates.push(card);
                continue;
            }
            if card.tags.iter().any(|tag| tag == "decision") {
                decisions.push(card.clone());
            }
            if card.tags.iter().any(|tag| tag == "wip") {
                wip.push(card.clone());
            }
            if card.tags.iter().any(|tag| tag == "lesson") {
                lessons.push(card.clone());
            }
            relevant.push(card);
        }
        let suggested_reads: Vec<MemoryId> = relevant.iter().take(5).map(|card| card.id).collect();
        if relevant.is_empty() && stale_candidates.is_empty() {
            warnings.push(quality_warning("empty_brief", "no visible memories matched the brief request"));
        }
        let unresolved_context_hints = scope_resolution.as_ref().is_some_and(|resolution| resolution.unresolved_scope) && !context_hints.is_empty();
        let no_matches = relevant.is_empty() && stale_candidates.is_empty();
        let stale_only_query = if relevant.is_empty() && !stale_candidates.is_empty() {
            supplied_query.as_deref()
        } else {
            None
        };
        let recommended_actions = brief_recommended_actions(&suggested_reads, unresolved_context_hints, no_matches, stale_only_query);
        success_json(&BriefResponse {
            relevant,
            decisions,
            wip,
            lessons,
            stale_candidates,
            suggested_reads,
            recommended_actions,
            scope_resolution,
            context_resolution: context_selection.resolution,
            warnings,
        })
    }

    #[tool(
        description = "Validate handoff candidate memories. candidates items may be string content shorthand or full objects; caps at max_batch_size; scope accepts key/alias/matcher/context_hints; previews by default and persists only when commit=true."
    )]
    async fn handoff(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<HandoffParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let commit = params.commit;
        let write_principal = if commit {
            match self.write_principal_for(request_principal.as_deref()) {
                Some(principal) => Some(principal),
                None => return Ok(Self::anonymous_write_denied()),
            }
        } else {
            if !self.read_allowed_for(request_principal.as_deref()) {
                return Ok(Self::anonymous_read_denied());
            }
            None
        };
        if let Some(error) = batch_len_tool_error(
            "candidates",
            params.candidates.len(),
            self.engine.limits().max_batch_size,
            "Pass one or more candidates, and split large handoff requests into smaller batches.",
        ) {
            return Ok(error);
        }
        let mut suggested_writes = Vec::with_capacity(params.candidates.len());
        let mut prepared = Vec::with_capacity(params.candidates.len());
        let mut all_warnings = Vec::new();
        let mut committed_count = 0_u64;
        let cleanup_principal = write_principal.as_deref().or(request_principal.as_deref()).unwrap_or(ANONYMOUS_PRINCIPAL);
        let mut created_legacy_contexts = Vec::new();
        for candidate in params.candidates.into_iter().map(HandoffCandidate::from) {
            let HandoffCandidate {
                content,
                summary,
                context: context_envelope,
                scope,
                context_hints,
                tags,
                entities,
                memory_type,
            } = candidate;
            let metadata_summary = trim_optional_text(summary.clone());
            let context_hints = match normalize_legacy_context_hints(context_hints) {
                Ok(hints) => hints,
                Err(error) => {
                    self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                    return Err(EngineError::from(error).into());
                }
            };
            if context_envelope.is_some() && (scope.is_some() || !context_hints.is_empty()) {
                self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                return Ok(tool_error(
                    ToolErrorCode::InvalidParams,
                    Some("context"),
                    "context cannot be combined with legacy scope or context_hints",
                    Some("Use the shared context envelope alone, or use only legacy scope/context_hints."),
                    false,
                ));
            }
            let memory_input = params::MemoryInput {
                content: content.clone(),
                tags: tags.clone(),
                source_agent: write_principal.clone().or_else(|| request_principal.clone()),
                source_conversation: Some(UNRESOLVED_SCOPE.to_owned()),
                origin_conversation: Some(UNRESOLVED_SCOPE.to_owned()),
                source_user: None,
                ttl_seconds: None,
                access_policy: None,
                memory_type,
                importance: None,
                confidence: None,
                supersedes: None,
                entities,
            };
            let now = self.engine.now();
            let input = match StoreMemoryInput::try_from(memory_input) {
                Ok(input) => input,
                Err(error) => {
                    self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                    return Err(EngineError::from(error).into());
                }
            };
            let supersedes = input.supersedes;
            let mut memory = match self.engine.build_memory(input, now) {
                Ok(memory) => memory,
                Err(error) => {
                    self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                    return Err(error.into());
                }
            };
            let resolution_principal = write_principal.as_deref().or(request_principal.as_deref()).unwrap_or(ANONYMOUS_PRINCIPAL);
            let (context_selection, scope_resolution, used_legacy_adapter) = if let Some(envelope) = context_envelope.as_ref() {
                let selection = match self.resolve_context_selection(resolution_principal, Some(envelope), None, true).await {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                        return Ok(error);
                    }
                };
                let resolved_scope = selection
                    .resolution
                    .direct
                    .first()
                    .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), |context| context.key.clone());
                (
                    selection,
                    ScopeResolution {
                        scope: resolved_scope.clone(),
                        unresolved_scope: resolved_scope == UNRESOLVED_SCOPE,
                        resolved_by: ScopeResolvedBy::Explicit,
                        matched_hint: None,
                        matched_value: None,
                    },
                    false,
                )
            } else if scope.is_some() || !context_hints.is_empty() {
                let scope_resolution = match self.resolve_scope(resolution_principal, scope.clone(), &context_hints).await {
                    Ok(resolution) => resolution,
                    Err(error) => {
                        self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                        return Err(error.into());
                    }
                };
                let selection = match self.resolve_legacy_context_selection(resolution_principal, &scope_resolution, commit).await {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                        return Ok(error);
                    }
                };
                let scope_resolution = canonicalize_legacy_scope_resolution(scope_resolution, &selection);
                (selection, scope_resolution, true)
            } else {
                let selection = match self.resolve_context_selection(resolution_principal, None, None, true).await {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                        return Ok(error);
                    }
                };
                let scope = selection
                    .resolution
                    .direct
                    .first()
                    .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), |context| context.key.clone());
                let unresolved = selection.direct_ids.is_empty();
                (
                    selection,
                    ScopeResolution {
                        scope,
                        unresolved_scope: unresolved,
                        resolved_by: if unresolved { ScopeResolvedBy::Unresolved } else { ScopeResolvedBy::Explicit },
                        matched_hint: None,
                        matched_value: None,
                    },
                    false,
                )
            };
            created_legacy_contexts.extend(context_selection.created_legacy_context);
            let resolved_scope = scope_resolution.scope.clone();
            let unresolved_scope = scope_resolution.unresolved_scope;
            memory.provenance.source_conversation = Some(resolved_scope.clone());
            memory.provenance.origin_conversation = Some(resolved_scope.clone());
            let warning_content = memory.content.as_str();
            let warning_tags = memory.tags.as_slice();
            let warning_entities_len = memory.entities.len();
            let mut warnings = write_quality_warnings(warning_content, summary.as_deref(), unresolved_scope, warning_tags, warning_entities_len);
            if used_legacy_adapter {
                warnings.push(quality_warning("legacy_scope_adapter", "legacy scope input was adapted to governed context membership"));
            }
            let duplicate_principal = write_principal.as_deref().or(request_principal.as_deref());
            let duplicate_candidates = self
                .duplicate_candidates(warning_content, Some(context_selection.effective_ids.clone()), duplicate_principal)
                .await
                .unwrap_or_default();
            if !duplicate_candidates.is_empty() {
                warnings.push(quality_warning(
                    "duplicate_candidate",
                    "similar memories already exist; review existing memories before committing this handoff candidate",
                ));
            }
            let quality_flags = warnings.iter().map(|warning| warning.code.clone()).collect();
            let next_action = next_action_for_warnings(&warnings);
            all_warnings.extend(warnings.clone());
            let write = commit.then(|| PreparedHandoffWrite {
                metadata: MemoryMetadata {
                    memory_id: memory.id,
                    scope_key: Some(resolved_scope.clone()),
                    summary: metadata_summary,
                    agent_label: None,
                    created_by_principal: write_principal.clone(),
                    quality_flags,
                    schema_version: 1,
                },
                memory,
                supersedes,
                context_ids: context_selection.direct_ids.clone(),
            });
            prepared.push(PreparedHandoff {
                suggestion: HandoffSuggestion {
                    content,
                    scope: resolved_scope,
                    unresolved_scope,
                    scope_resolution,
                    context_resolution: Some(context_selection.resolution.clone()),
                    contexts: context_selection.resolution.direct,
                    warnings,
                    id: None,
                    duplicate_candidates,
                    next_action,
                },
                write,
            });
        }

        if commit {
            let mut memories = Vec::with_capacity(prepared.len());
            let mut supersedes = Vec::with_capacity(prepared.len());
            let mut metadata = Vec::with_capacity(prepared.len());
            let mut context_ids = Vec::with_capacity(prepared.len());
            let mut suggestion_indexes = Vec::with_capacity(prepared.len());

            for (index, item) in prepared.iter_mut().enumerate() {
                if let Some(write) = item.write.take() {
                    suggestion_indexes.push(index);
                    supersedes.push(write.supersedes);
                    memories.push(write.memory);
                    metadata.push(write.metadata);
                    context_ids.push(write.context_ids);
                }
            }

            let ids = match self.engine.batch_store_with_metadata_contexts(memories, supersedes, metadata, context_ids).await {
                Ok(ids) => ids,
                Err(error) => {
                    self.rollback_created_legacy_contexts(&mut created_legacy_contexts, cleanup_principal).await?;
                    return Err(error.into());
                }
            };
            if ids.len() != suggestion_indexes.len() {
                return Err(rmcp::ErrorData::internal_error("handoff batch store returned an unexpected ID count".to_owned(), None));
            }
            for (index, id) in suggestion_indexes.into_iter().zip(ids.iter().copied()) {
                prepared[index].suggestion.id = Some(id);
            }
            committed_count = u64::try_from(ids.len()).map_err(|_err| rmcp::ErrorData::internal_error("handoff committed count overflowed u64".to_owned(), None))?;
        }

        for item in prepared {
            suggested_writes.push(item.suggestion);
        }

        let status = if commit {
            if committed_count == 0 { OperationStatus::NoOp } else { OperationStatus::Applied }
        } else {
            OperationStatus::Preview
        };
        let next_action = if commit { NextAction::None } else { NextAction::Continue };
        success_json(&HandoffResponse {
            operation: operation_summary(status, committed_count, all_warnings, next_action),
            committed: commit,
            suggested_writes,
        })
    }

    #[tool(
        description = "Write/admin: register or replace a scope definition using the server-resolved principal. Defines scope_key plus aliases and matcher substrings for context_hints."
    )]
    async fn admin_scope_register(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminScopeRegisterParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let scope_key = normalize_non_empty("scope_key", &params.scope_key).map_err(EngineError::from)?;
        let display_name = normalize_non_empty("display_name", &params.display_name).map_err(EngineError::from)?;
        let aliases = normalize_optional_string_array("aliases", Some(params.aliases))
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let matchers = normalize_optional_string_array("matchers", Some(params.matchers))
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let related = normalize_optional_string_array("related", Some(params.related))
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let scope = ScopeDefinition {
            scope_key: scope_key.clone(),
            display_name,
            description: normalize_optional_non_empty("description", params.description).map_err(EngineError::from)?,
            aliases,
            matchers,
            parent: normalize_optional_non_empty("parent", params.parent).map_err(EngineError::from)?,
            related,
        };
        if let Err(message) = validate_legacy_scope_definition(&scope) {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("admin_scope_register"),
                message,
                Some("Reduce legacy scope keys, descriptions, aliases, matchers, and related entries to the documented context limits."),
                false,
            ));
        }
        self.engine.register_scope_for_principal(scope.clone(), &principal).await?;
        success_json(&AdminScopeRegisterResponse { scope: ScopeEntry::from(scope) })
    }

    #[tool(description = "Read-like admin: list persisted scope definitions using read authorization from the server-resolved principal.")]
    async fn admin_scope_list(&self, context: RequestContext<RoleServer>, Parameters(_params): Parameters<AdminScopeListParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let scopes = self
            .engine
            .list_scopes_for_principal(request_principal.as_deref().unwrap_or(ANONYMOUS_PRINCIPAL))
            .await?
            .into_iter()
            .map(ScopeEntry::from)
            .collect();
        success_json(&AdminScopeListResponse { scopes })
    }

    #[tool(description = "Read-like admin: list compact inventory cards for memories visible to the server-resolved principal without returning full content.")]
    async fn admin_list(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminListParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let expand_scopes = params.expand_scopes.unwrap_or(true);
        let common = match self.common_filter_from_admin(params.filter, request_principal.as_deref(), expand_scopes).await {
            Ok(common) => common,
            Err(error) => return error.into_tool_result(),
        };
        let (mut filter, ctx) = validate_and_normalize_filter(&common)?;
        filter.text_search = normalize_text_search(params.text_search)?;
        filter.has_embedding = params.has_embedding;
        filter.limit = params.limit;
        maybe_expand_scope_hierarchy(&mut filter, expand_scopes).map_err(EngineError::from)?;

        let now = self.engine.now();
        let memories = self.engine.list_memories(filter, ctx).await?;
        let views = self.memory_views(memories, request_principal.as_deref()).await?;
        let mut cards = Vec::with_capacity(views.len());
        for view in views {
            cards.push(inventory_card_from_view(view, now));
        }
        success_json(&AdminListResponse {
            count: cards.len(),
            memories: cards,
            warnings: Vec::new(),
        })
    }

    #[tool(description = "Write/admin: report conservative metadata migration counts using the server-resolved principal; does not rewrite original memory content.")]
    async fn admin_migration_report(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(_params): Parameters<AdminMigrationReportParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if let Some(error) = self.local_admin_error_for_context(&context) {
            return Ok(error);
        }
        let report = self.engine.metadata_migration_report().await?;
        success_json(&AdminMigrationReportResponse { report })
    }

    #[tool(
        description = "Write/admin: add metadata rows for existing memories using the server-resolved principal. dry_run=true previews; original memory content is never rewritten."
    )]
    async fn admin_migrate_metadata(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(params): Parameters<AdminMigrateMetadataParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if let Some(error) = self.local_admin_error_for_context(&context) {
            return Ok(error);
        }
        let Some(principal) = self.write_principal_for(self.principal_for_context(&context).as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let registered_scope_keys = self
            .engine
            .list_scopes_for_principal(&principal)
            .await?
            .into_iter()
            .map(|scope| scope.scope_key)
            .collect::<Vec<_>>();
        let report = self.engine.migrate_metadata(&registered_scope_keys, params.dry_run, &principal).await?;
        let status = if params.dry_run { OperationStatus::Preview } else { OperationStatus::Applied };
        success_json(&AdminMigrateMetadataResponse {
            operation: operation_summary(status, report.migrated, Vec::new(), NextAction::None),
            dry_run: params.dry_run,
            report,
        })
    }

    #[tool(
        description = "Write/admin: reassign memory scope using the server-resolved principal after checking write access per memory. from_scope/to_scope are scope keys; origin_scope is an optional origin filter."
    )]
    async fn admin_reassign_scope(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminReassignScopeParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let from_scope = normalize_non_empty("from_scope", &params.from_scope).map_err(EngineError::from)?;
        let to_scope = normalize_non_empty("to_scope", &params.to_scope).map_err(EngineError::from)?;
        validate_legacy_scope_key(&from_scope).map_err(|message| EngineError::from(crate::error::ValidationError::new("from_scope", message)))?;
        validate_implicit_legacy_context_key(&to_scope).map_err(|message| EngineError::from(crate::error::ValidationError::new("to_scope", message)))?;
        reject_removed_admin_field(params.deprecated_origin_conversation.is_some(), "origin_conversation", "origin_scope")?;
        let origin_scope = normalize_legacy_scope_value("origin_scope", params.origin_scope).map_err(EngineError::from)?;

        let reassigned = self.engine.reassign_scope(&from_scope, &to_scope, origin_scope.as_deref(), &principal).await?;
        success_json(&ReassignScopeResponse {
            operation: operation_summary(OperationStatus::Applied, reassigned, Vec::new(), NextAction::None),
            reassigned,
        })
    }

    #[tool(
        description = "Write/admin: destructively evict expired TTL memories using the server-resolved principal. mode defaults to authorized per-memory write policy; mode=all is explicit whole-store maintenance restricted to authenticated local stdio."
    )]
    async fn admin_cleanup_expired(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(params): Parameters<AdminCleanupExpiredParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let mode = params.mode;
        if matches!(mode, params::AdminCleanupExpiredMode::All)
            && let Some(error) = self.local_admin_error_for_context(&context)
        {
            return Ok(error);
        }
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let deleted = match mode {
            params::AdminCleanupExpiredMode::Authorized => self.engine.evict_expired(&principal).await?,
            params::AdminCleanupExpiredMode::All => self.engine.evict_expired_all(&principal).await?,
        };
        tracing::info!(principal = principal.as_str(), mode = mode.as_str(), deleted, "admin_cleanup_expired completed");
        success_json(&EvictExpiredResponse {
            operation: operation_summary(OperationStatus::Applied, deleted, Vec::new(), NextAction::None),
            deleted,
        })
    }

    #[tool(description = "Read-like admin: return aggregate memory statistics for memories visible to the server-resolved principal.")]
    async fn admin_count(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminCountParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let expand_scopes = params.expand_scopes.unwrap_or(true);
        let common = match self.common_filter_from_admin(params.filter, request_principal.as_deref(), expand_scopes).await {
            Ok(common) => common,
            Err(error) => return error.into_tool_result(),
        };
        let (mut filter, ctx) = validate_and_normalize_filter(&common)?;
        maybe_expand_scope_hierarchy(&mut filter, expand_scopes).map_err(EngineError::from)?;
        let stats = self.engine.count_memories(filter, ctx, params.top_tags_limit).await?;

        let response = CountResponse {
            total: stats.total,
            with_embedding: stats.with_embedding,
            without_embedding: stats.without_embedding,
            expired: stats.expired,
            by_tag: stats.by_tag.into_iter().map(|(tag, count)| TagCount { tag, count }).collect(),
            by_agent_label: stats.by_agent_label.into_iter().map(|(agent_label, count)| AgentCount { agent_label, count }).collect(),
            storage_bytes: stats.storage_bytes,
            oldest_memory: stats.oldest_memory.map(|dt| dt.to_rfc3339()),
            newest_memory: stats.newest_memory.map(|dt| dt.to_rfc3339()),
            scope_count: stats.scope_count,
            by_scope: stats.by_scope.into_iter().map(|(scope, count)| ScopeCount { scope, count }).collect(),
            by_memory_type: stats
                .by_memory_type
                .into_iter()
                .map(|(memory_type, count)| params::MemoryTypeCount { memory_type, count })
                .collect(),
            superseded_count: stats.superseded_count,
        };
        success_json(&response)
    }

    #[tool(
        description = "Write/admin: destructively delete memories matching filters using the server-resolved principal after checking write access per memory; reports matched/deleted/capped."
    )]
    async fn admin_bulk_delete(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminBulkDeleteParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let expand_scopes = params.expand_scopes.unwrap_or(true);
        let common = match self.common_filter_from_admin(params.filter, Some(&principal), expand_scopes).await {
            Ok(common) => common,
            Err(error) => return error.into_tool_result(),
        };
        let (mut filter, mut ctx) = validate_and_normalize_filter(&common)?;
        let includes_contextless = !common.explicit_context_filter;
        if !common.explicit_context_filter {
            filter.context_ids = None;
        }
        ctx.principal = Some(principal.clone());
        filter.text_search = normalize_text_search(params.text_search)?;
        maybe_expand_scope_hierarchy(&mut filter, expand_scopes).map_err(EngineError::from)?;

        let result = self.engine.bulk_delete(&principal, filter, ctx).await?;
        let warnings = if includes_contextless {
            vec![quality_warning(
                "contextless_maintenance_scope",
                "No context filter was supplied, so this maintenance operation also considered memories with no active context, including explicitly deferred and archived-only rows that broad admin reads omit.",
            )]
        } else {
            Vec::new()
        };
        let next_action = next_action_for_warnings(&warnings);
        let mut operation = operation_summary(OperationStatus::Applied, result.deleted, warnings, next_action);
        operation.matched = Some(result.matched);
        operation.capped = result.capped;
        success_json(&BulkDeleteResponse {
            operation,
            deleted: result.deleted,
            matched: result.matched,
            capped: result.capped,
        })
    }

    #[tool(
        description = "Write/admin: update metadata fields on memories matching filters using the server-resolved principal after checking write access per memory; access_policy accepts shorthand or full object."
    )]
    async fn admin_bulk_update(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminBulkUpdateParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let expand_scopes = params.expand_scopes.unwrap_or(true);
        let common = match self.common_filter_from_admin(params.filter, Some(&principal), expand_scopes).await {
            Ok(common) => common,
            Err(error) => return error.into_tool_result(),
        };
        let (mut filter, mut ctx) = validate_and_normalize_filter(&common)?;
        let includes_contextless = !common.explicit_context_filter;
        if !common.explicit_context_filter {
            filter.context_ids = None;
        }
        ctx.principal = Some(principal.clone());
        filter.text_search = normalize_text_search(params.text_search)?;
        maybe_expand_scope_hierarchy(&mut filter, expand_scopes).map_err(EngineError::from)?;

        let set_tags = normalize_optional_string_array("set_tags", params.set_tags).map_err(EngineError::from)?;
        let fields = BulkUpdateFields {
            tags: set_tags,
            importance: params.importance.map(crate::types::Importance::new),
            access_policy: normalize_optional_access_policy(params.access_policy),
        };

        let result = self.engine.bulk_update(&principal, filter, ctx, fields).await?;
        let warnings = if includes_contextless {
            vec![quality_warning(
                "contextless_maintenance_scope",
                "No context filter was supplied, so this maintenance operation also considered memories with no active context, including explicitly deferred and archived-only rows that broad admin reads omit.",
            )]
        } else {
            Vec::new()
        };
        let next_action = next_action_for_warnings(&warnings);
        let mut operation = operation_summary(OperationStatus::Applied, result.updated, warnings, next_action);
        operation.matched = Some(result.matched);
        operation.denied = Some(result.denied);
        operation.capped = result.capped;
        success_json(&BulkUpdateResponse {
            operation,
            updated: result.updated,
            denied: result.denied,
            matched: result.matched,
            capped: result.capped,
        })
    }

    #[tool(
        description = "Write/admin: find near-duplicate memories using the server-resolved principal. dry_run=true previews; dry_run=false merges by superseding duplicates. Reports when the configured candidate work limit makes the run partial."
    )]
    async fn admin_consolidate(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminConsolidateParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        reject_removed_admin_field(params.deprecated_scope_keys_any.is_some(), "scope_keys_any", "scopes")?;
        let scope = normalize_legacy_scope_value("scope", params.scope).map_err(EngineError::from)?;
        let scopes = normalize_legacy_scope_values("scopes", params.scopes).map_err(EngineError::from)?;
        if params.context.is_some() && (scope.is_some() || !scopes.is_empty()) {
            return Ok(tool_error(
                ToolErrorCode::InvalidParams,
                Some("context"),
                "context cannot be combined with legacy scope or scopes",
                Some("Use the shared context envelope alone, or use only legacy scope/scopes."),
                false,
            ));
        }
        let (context_ids, legacy_context_ids_any) = if let Some(envelope) = params.context.as_ref() {
            let selection = match self.resolve_context_selection(&principal, Some(envelope), None, false).await {
                Ok(selection) => selection,
                Err(error) => return Ok(error),
            };
            (selection.effective_ids, None)
        } else if !scopes.is_empty() {
            let mut values = scope.into_iter().collect::<Vec<_>>();
            values.extend(scopes);
            let ids = match self.resolve_admin_legacy_context_ids(&principal, values, false).await {
                Ok(ids) => ids,
                Err(error) => return error.into_tool_result(),
            };
            (Vec::new(), Some(ids))
        } else if let Some(scope) = scope {
            let ids = match self.resolve_admin_legacy_context_ids(&principal, vec![scope], false).await {
                Ok(ids) => ids,
                Err(error) => return error.into_tool_result(),
            };
            (ids, None)
        } else {
            (Vec::new(), None)
        };

        let result = self
            .engine
            .consolidate_memories(
                &principal,
                &context_ids,
                legacy_context_ids_any.as_deref(),
                params.similarity_threshold,
                params.limit,
                params.dry_run,
            )
            .await?;

        let mut operation = operation_summary(
            if params.dry_run { OperationStatus::Preview } else { OperationStatus::Applied },
            u64::from(!params.dry_run && result.merged),
            Vec::new(),
            if params.dry_run { NextAction::Continue } else { NextAction::None },
        );
        operation.capped = result.capped;
        let response = ConsolidateResponse {
            operation,
            groups: result
                .groups
                .into_iter()
                .map(|g| DuplicateGroupEntry {
                    representative_id: g.representative_id,
                    member_ids: g.member_ids,
                    similarity: g.similarity,
                    member_count: g.member_count,
                })
                .collect(),
            merged: result.merged,
            candidate_count: result.candidate_count,
            capped: result.capped,
        };
        success_json(&response)
    }

    #[tool(
        description = "Read-like admin: query transactional mutation audit history for one memory visible to the server-resolved principal without returning raw memory content."
    )]
    async fn admin_history(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminHistoryParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        tracing::debug!(principal = request_principal.as_deref(), memory_id = %params.id, "admin_history requested");
        let history_access = if let Some(memory) = self.engine.get_memory(&params.id, request_principal.as_deref()).await? {
            if memory.was_redacted { AccessLevel::Redacted } else { AccessLevel::Full }
        } else if let Some(tombstone) = self.engine.get_tombstone(&params.id).await? {
            tombstone.check_access_level(request_principal.as_deref())
        } else {
            AccessLevel::Denied
        };
        let mut entries = self.engine.query_audit_log(&params.id, params.limit).await?;
        if history_access == AccessLevel::Denied {
            entries.clear();
        }
        let redacted_history_view = history_access == AccessLevel::Redacted;

        let response = HistoryResponse {
            entries: entries
                .into_iter()
                .map(|e| AuditEntryResponse {
                    action: e.action,
                    principal: (!redacted_history_view).then_some(e.caller_agent).flatten(),
                    timestamp: e.timestamp,
                    details: (!redacted_history_view).then_some(e.details).flatten(),
                })
                .collect(),
        };
        success_json(&response)
    }

    #[tool(
        description = "Write/admin: trigger re-embedding for one memory or a capped batch of unembedded memories after checking write access for the server-resolved principal."
    )]
    async fn admin_reembed(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminReembedParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let bulk_limit = params.limit.unwrap_or_else(|| self.engine.limits().max_reembed_limit);
        if params.id.is_none()
            && let Some(error) = batch_len_tool_error(
                "limit",
                bulk_limit,
                self.engine.limits().max_reembed_limit,
                "Pass a positive limit within max_reembed_limit, or split re-embed work into smaller batches.",
            )
        {
            return Ok(error);
        }
        let request = match params.id {
            Some(id) => ReembedRequest::Single { id, principal },
            None => ReembedRequest::Bulk { limit: bulk_limit, principal },
        };

        let outcome = self.engine.reembed(request).await?;
        match outcome {
            ReembedOutcome::Queued(queued) => {
                let changed = u64::try_from(queued).map_err(|_err| rmcp::ErrorData::internal_error("reembed queued count overflowed u64".to_owned(), None))?;
                success_json(&ReembedResponse {
                    operation: operation_summary(OperationStatus::Queued, changed, Vec::new(), NextAction::None),
                    queued,
                })
            }
            ReembedOutcome::NotFound(id) => Ok(tool_error(
                ToolErrorCode::NotFound,
                Some("id"),
                format!("memory not found or not authorized: {id}"),
                Some("Check the memory ID and principal access before retrying."),
                false,
            )),
        }
    }
}

impl TryFrom<params::MemoryInput> for StoreMemoryInput {
    type Error = crate::error::ValidationError;

    fn try_from(params: params::MemoryInput) -> Result<Self, Self::Error> {
        Ok(Self {
            content: params.content,
            tags: params.tags,
            source_agent: params.source_agent,
            source_user: params.source_user,
            source_conversation: params.source_conversation,
            origin_conversation: params.origin_conversation,
            access_policy: normalize_optional_access_policy(params.access_policy),
            ttl_seconds: params.ttl_seconds,
            memory_type: params.memory_type,
            importance: params.importance,
            confidence: params.confidence,
            supersedes: params.supersedes,
            entities: normalize_entity_inputs(params.entities)?,
        })
    }
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> ServerHandler for LocalHoldServer<S> {
    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResult, rmcp::ErrorData> {
        let context = ToolCallContext::new(self, request, context);
        self.tool_router.call(context).await
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, rmcp::ErrorData> {
        let tools = DEFAULT_DISCOVERY_TOOLS.iter().filter_map(|name| self.tool_router.get(name).cloned()).collect();
        Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info = Implementation::new("localhold", env!("CARGO_PKG_VERSION")).with_title("LocalHold");
        let admin_instructions = if self.tool_router.get("admin_list").is_some() {
            " Privileged admin tools are enabled for migration, repair, statistics, re-embedding, consolidation, context compatibility administration, and audit history."
        } else {
            " Privileged admin tools are disabled by default and require explicit operator configuration."
        };
        info.instructions = Some(format!(
            "LocalHold is a deterministic local memory server. For normal agent work, start with \
             brief, use recall to get compact relevant cards, read or read_many to fetch full memory \
             content and record activity, and remember to store durable new information. Use context_resolve \
             to select governed contexts and context_create when policy permits; governed writes need a resolved \
             context, a policy default, or explicit unresolved deferral. Use handoff to validate \
             candidate memories before persisting them. revise and forget modify existing memories \
             using the server-resolved principal.{admin_instructions} \
             Retired memory_* names are not part of the public MCP tool surface."
        ));
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[cfg(test)]
mod tests;
