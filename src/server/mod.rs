//! MCP protocol handlers — tool routing, request dispatch, and response formatting.

/// MCP tool parameter and response types.
pub mod params;

use std::sync::Arc;

use axum::http::request::Parts;
use params::{
    AdminBulkDeleteParams, AdminBulkUpdateParams, AdminCleanupExpiredParams, AdminConsolidateParams, AdminCountParams, AdminFilterFields, AdminHistoryParams, AdminListParams,
    AdminListResponse, AdminMigrateMetadataParams, AdminMigrateMetadataResponse, AdminMigrationReportParams, AdminMigrationReportResponse, AdminReassignScopeParams,
    AdminReembedParams, AdminScopeListParams, AdminScopeListResponse, AdminScopeRegisterParams, AdminScopeRegisterResponse, AgentCount, AuditEntryResponse, BriefParams,
    BriefResponse, BulkDeleteResponse, BulkUpdateResponse, ConsolidateResponse, CountResponse, DeleteResponse, DuplicateCandidateCard, DuplicateGroupEntry, EvictExpiredResponse,
    ForgetParams, HandoffCandidate, HandoffParams, HandoffResponse, HandoffSuggestion, HistoryResponse, InventoryCard, MatchAction, MatchAssessment, MatchDiagnostics,
    MatchQuality, MatchScoreBasis, MemoryEntry, NextAction, OperationStatus, OperationSummary, QualityWarning, QualityWarningSeverity, ReadManyItemResponse, ReadManyParams,
    ReadManyResponse, ReadManyStatus, ReadParams, ReadResponse, ReassignScopeResponse, RecallCard, RecallParams, RecallResponse, RecommendedAction, RecommendedActionPriority,
    RecommendedActionTool, ReembedResponse, RememberManyItemResponse, RememberManyParams, RememberManyResponse, RememberParams, RememberResponse, ReviseParams, ScopeCount,
    ScopeEntry, ScopeResolution, ScopeResolvedBy, TagCount, ToolError, ToolErrorCode, ToolErrorResponse, UpdateResponse,
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
    embedding::EmbeddingProvider,
    engine::{BulkUpdateFields, LocalHoldEngine, ReembedOutcome, ReembedRequest, SearchRequest, StoreMemoryInput},
    error::EngineError,
    store::{MemoryStore, RecordUseOutcome},
    types::{
        AccessLevel, LARGE_CONTENT_WARNING_THRESHOLD_BYTES, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryUpdate, MetadataPatch, QueryContext, RedactableField,
        ScopeDefinition, WriteOutcome,
    },
    validation::{normalize_non_empty, normalize_optional_non_empty, normalize_optional_string_array, validate_batch_len, validate_optional_non_empty},
};

const UNRESOLVED_SCOPE: &str = "inbox/unresolved";
const REDACTED_SCOPE: &str = "[redacted]";
const SERVER_PRINCIPAL: &str = "stdio";
const HTTP_PRINCIPAL: &str = "http";
const ANONYMOUS_PRINCIPAL: &str = "anonymous";
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
}

struct PreparedRemember {
    memory: Memory,
    supersedes: Option<MemoryId>,
    metadata: MemoryMetadata,
    scope_resolution: ScopeResolution,
    duplicate_candidates: Vec<DuplicateCandidateCard>,
    warnings: Vec<QualityWarning>,
}

enum PrepareRememberError {
    Invalid {
        error: crate::error::ValidationError,
        suggested_fix: &'static str,
    },
    Engine(EngineError),
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
        }
    }
}

struct PreparedHandoffWrite {
    memory: Memory,
    supersedes: Option<MemoryId>,
    metadata: MemoryMetadata,
}

struct PreparedHandoff {
    suggestion: HandoffSuggestion,
    write: Option<PreparedHandoffWrite>,
}

impl MemoryView {
    const fn new(memory: Memory, metadata: Option<MemoryMetadata>) -> Self {
        Self { memory, metadata }
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
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.scope_key.clone())
            .or_else(|| self.memory.provenance.source_conversation.clone())
    }

    fn card_scope(&self) -> String {
        self.scope()
            .unwrap_or_else(|| if self.is_redacted() { REDACTED_SCOPE.to_owned() } else { UNRESOLVED_SCOPE.to_owned() })
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

    fn caller_context_for(principal: Option<&str>) -> QueryContext {
        QueryContext {
            principal: principal.map(ToOwned::to_owned),
        }
    }

    fn principal_for_context(&self, context: &RequestContext<RoleServer>) -> Option<String> {
        if context.extensions.get::<Parts>().is_some() {
            return self.http_principal_for_context(context);
        }
        self.principal().map(ToOwned::to_owned)
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
        if self.principal().is_none() {
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

    async fn memory_view(&self, memory: Memory) -> Result<MemoryView, EngineError> {
        let metadata = self.engine.get_metadata(&memory.id).await?;
        Ok(MemoryView::new(memory, metadata))
    }

    async fn recall_card(&self, result: crate::types::SearchResult, reranker_blend_weight: f64) -> Result<RecallCard, EngineError> {
        let r#match = match_assessment(&result, reranker_blend_weight);
        let diagnostics = if result.memory.was_redacted {
            MatchDiagnostics::default()
        } else {
            match_diagnostics(&result, reranker_blend_weight)
        };
        let view = self.memory_view(result.memory).await?;
        Ok(recall_card_from_view(view, r#match, diagnostics))
    }

    async fn duplicate_card(&self, result: crate::types::SearchResult, reranker_blend_weight: f64) -> Result<DuplicateCandidateCard, EngineError> {
        let r#match = match_assessment(&result, reranker_blend_weight);
        let view = self.memory_view(result.memory).await?;
        Ok(DuplicateCandidateCard {
            id: view.memory.id,
            summary_or_excerpt: view.summary_or_excerpt(),
            r#match,
        })
    }

    async fn inventory_card(&self, memory: Memory, now: chrono::DateTime<chrono::Utc>) -> Result<InventoryCard, EngineError> {
        let view = self.memory_view(memory).await?;
        Ok(inventory_card_from_view(view, now))
    }

    async fn full_read_item(&self, id: MemoryId, mem: Memory, activity_recorded: bool) -> Result<ReadManyItemResponse, EngineError> {
        let view = self.memory_view(mem).await?;
        let summary = view.summary();
        let scope = view.scope();
        let agent_label = view.agent_label();
        let created_by_principal = view.created_by_principal();
        let quality_flags = view.quality_flags();
        let unresolved_scope = view.unresolved_scope();
        Ok(ReadManyItemResponse {
            id,
            status: ReadManyStatus::Found,
            memory: Some(MemoryEntry::from(view.memory.sanitize_for_wire())),
            summary,
            scope,
            agent_label,
            created_by_principal,
            quality_flags,
            unresolved_scope,
            activity_recorded,
        })
    }

    #[expect(clippy::too_many_lines, reason = "scope-resolution diagnostics are clearer when the ordered resolution branches stay together")]
    async fn resolve_scope(&self, explicit_scope: Option<String>, context_hints: &[String]) -> Result<ScopeResolution, EngineError> {
        let explicit_scope = normalize_optional_non_empty("scope", explicit_scope).map_err(EngineError::from)?;
        if explicit_scope.is_none() && context_hints.is_empty() {
            return Ok(ScopeResolution {
                scope: UNRESOLVED_SCOPE.to_owned(),
                unresolved_scope: true,
                resolved_by: ScopeResolvedBy::Unresolved,
                matched_hint: None,
                matched_value: None,
            });
        }
        let registry = self.engine.list_scopes().await?;
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

    async fn resolve_admin_scope_filter(&self, scope: Option<String>, scopes: Option<Vec<String>>) -> Result<Option<Vec<String>>, EngineError> {
        let mut resolved = Vec::new();
        if let Some(scope) = normalize_optional_non_empty("scope", scope).map_err(EngineError::from)? {
            let resolution = self.resolve_scope(Some(scope), &[]).await?;
            resolved.push(resolution.scope);
        }
        let scopes = normalize_optional_string_array("scopes", scopes).map_err(EngineError::from)?.unwrap_or_default();
        for scope in scopes {
            let scope_key = self.resolve_scope(Some(scope), &[]).await?.scope;
            if resolved.contains(&scope_key) {
                continue;
            }
            resolved.push(scope_key);
        }
        Ok((!resolved.is_empty()).then_some(resolved))
    }

    async fn common_filter_from_admin(&self, fields: AdminFilterFields, principal: Option<&str>) -> Result<params::CommonFilterFields, EngineError> {
        reject_removed_admin_field(fields.deprecated_source_agent.is_some(), "source_agent", "agent_label")?;
        reject_removed_admin_field(fields.deprecated_source_conversation.is_some(), "source_conversation", "scope")?;
        reject_removed_admin_field(fields.deprecated_origin_conversation.is_some(), "origin_conversation", "origin_scope")?;
        reject_removed_admin_field(fields.deprecated_scope_keys_any.is_some(), "scope_keys_any", "scopes")?;
        let scopes_any = self.resolve_admin_scope_filter(fields.scope, fields.scopes).await?;
        let agent_label = normalize_optional_non_empty("agent_label", fields.agent_label).map_err(EngineError::from)?;
        let origin_scope = normalize_optional_non_empty("origin_scope", fields.origin_scope).map_err(EngineError::from)?;
        Ok(params::CommonFilterFields {
            tags: fields.tags,
            agent_label,
            scope: None,
            origin_scope,
            scopes_any,
            principal: principal.map(ToOwned::to_owned),
            memory_type: fields.memory_type,
            include_superseded: fields.include_superseded,
            entity: fields.entity,
            entity_type: fields.entity_type,
        })
    }

    async fn duplicate_candidates(&self, content: &str, scope: &str, principal: Option<&str>) -> Result<Vec<DuplicateCandidateCard>, EngineError> {
        let mut filter = MemoryFilter::default();
        if scope != UNRESOLVED_SCOPE {
            filter.scopes_any = Some(vec![scope.to_owned()]);
        }
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
        let mut cards = Vec::with_capacity(outcome.results.len());
        let reranker_blend_weight = self.engine.search_config().reranker.blend_weight;
        for result in outcome.results {
            let card = self.duplicate_card(result, reranker_blend_weight).await?;
            cards.push(card);
        }
        Ok(cards)
    }

    async fn prepare_remember(&self, params: RememberParams, principal: String, now: chrono::DateTime<chrono::Utc>) -> Result<PreparedRemember, PrepareRememberError> {
        let summary = trim_optional_text(params.summary);
        let agent_label = trim_optional_text(params.agent_label);
        let context_hints = normalize_optional_string_array("context_hints", Some(params.context_hints))
            .map_err(|error| PrepareRememberError::invalid(error, "Remove blank values from context_hints."))?
            .unwrap_or_default();
        let scope_resolution = self.resolve_scope(params.scope, &context_hints).await.map_err(PrepareRememberError::Engine)?;
        let scope = scope_resolution.scope.clone();
        let unresolved_scope = scope_resolution.unresolved_scope;
        let memory_input = params::MemoryInput {
            content: params.content,
            tags: params.tags,
            source_agent: Some(principal.clone()),
            source_conversation: Some(scope.clone()),
            origin_conversation: Some(scope.clone()),
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
        let memory = self.engine.build_memory(input, now).map_err(|error| match error {
            EngineError::Validation(error) => PrepareRememberError::invalid(error, "Provide valid memory content and metadata."),
            engine_error @ (EngineError::Config(_)
            | EngineError::Store(_)
            | EngineError::EmbeddingUnavailable(_)
            | EngineError::SearchUnavailable(_)
            | EngineError::Embedding(_)
            | EngineError::ShuttingDown) => PrepareRememberError::Engine(engine_error),
        })?;
        let mut warnings = write_quality_warnings(&memory.content, summary.as_deref(), unresolved_scope, &memory.tags, memory.entities.len());
        let duplicate_candidates = self.duplicate_candidates(&memory.content, &scope, Some(&principal)).await.unwrap_or_default();
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
            duplicate_candidates,
            warnings,
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
    let response = ToolErrorResponse {
        error: ToolError {
            code,
            field: field.map(ToOwned::to_owned),
            message: message.into(),
            suggested_fix: suggested_fix.map(ToOwned::to_owned),
            retryable,
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
    let memory = view.memory;
    RecallCard {
        id: memory.id,
        summary_or_excerpt,
        scope,
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
    let memory = view.memory;
    let expired = memory.expires_at.is_some_and(|expires_at| expires_at <= now);
    let superseded = memory.superseded_by.is_some();
    InventoryCard {
        id: memory.id,
        summary_or_excerpt,
        scope,
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

/// Expand a single scope key into all ancestor scopes by splitting on `/`.
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

/// Expand all scope keys in a list to include their ancestor scopes, deduplicating.
fn expand_scope_keys(scope_keys: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    scope_keys
        .iter()
        .flat_map(|key| expand_scope_hierarchy(key))
        .filter(|ancestor| seen.insert(ancestor.clone()))
        .collect()
}

/// Optionally expand scope hierarchy on a filter's `scopes_any`.
///
/// When `expand` is `true`, each scope key is expanded to include all ancestor
/// scopes (e.g. `"a/b/c"` also matches `"a/b"` and `"a"`). The filter is
/// mutated in place only when expansion actually adds new keys.
fn maybe_expand_scope_hierarchy(filter: &mut MemoryFilter, expand: bool) {
    if expand && let Some(scope_keys) = &filter.scopes_any {
        let expanded = expand_scope_keys(scope_keys);
        if expanded.len() != scope_keys.len() {
            filter.scopes_any = Some(expanded);
        }
    }
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
        let scope = normalize_optional_non_empty("scope", fields.scope.clone())?;
        let origin_scope = normalize_optional_non_empty("origin_scope", fields.origin_scope.clone())?;
        let scopes_any = normalize_optional_string_array("scopes_any", fields.scopes_any.clone())?;
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
        description = "Remember durable information. Write authorization uses the server-resolved principal; scope may be a key, alias, matcher-containing value, or context_hints; entities/access_policy accept shorthand or full objects."
    )]
    async fn remember(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<RememberParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let prepared = match self.prepare_remember(params, principal, self.engine.now()).await {
            Ok(prepared) => prepared,
            Err(error) => return error.into_tool_result(None),
        };
        let PreparedRemember {
            memory,
            supersedes,
            metadata,
            scope_resolution,
            duplicate_candidates,
            warnings,
        } = prepared;
        let scope = scope_resolution.scope.clone();
        let unresolved_scope = scope_resolution.unresolved_scope;
        let id = self.engine.store_memory_with_metadata(memory, supersedes.as_ref(), &metadata).await?;
        let next_action = next_action_for_warnings(&warnings);
        success_json(&RememberResponse {
            operation: operation_summary(OperationStatus::Applied, 1, warnings.clone(), next_action),
            id,
            scope,
            unresolved_scope,
            scope_resolution,
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
        let mut memories = Vec::with_capacity(params.memories.len());
        let mut supersedes_list = Vec::with_capacity(params.memories.len());
        let mut metadata = Vec::with_capacity(params.memories.len());
        let mut item_responses = Vec::with_capacity(params.memories.len());
        let mut all_warnings = Vec::new();

        for (index, params) in params.memories.into_iter().map(RememberParams::from).enumerate() {
            let prepared = match self.prepare_remember(params, principal.clone(), now).await {
                Ok(prepared) => prepared,
                Err(error) => return error.into_tool_result(Some(index)),
            };
            let PreparedRemember {
                memory,
                supersedes,
                metadata: metadata_item,
                scope_resolution,
                duplicate_candidates,
                warnings,
            } = prepared;
            let scope = scope_resolution.scope.clone();
            let unresolved_scope = scope_resolution.unresolved_scope;
            memories.push(memory);
            supersedes_list.push(supersedes);
            metadata.push(metadata_item);
            all_warnings.extend(warnings.iter().cloned());
            item_responses.push(RememberManyItemResponse {
                id: MemoryId::new(),
                scope,
                unresolved_scope,
                scope_resolution,
                duplicate_candidates,
                warnings,
            });
        }

        let ids = self.engine.batch_store_with_metadata(memories, supersedes_list, metadata).await?;
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
        let context_hints = normalize_optional_string_array("context_hints", Some(params.context_hints))
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let mut warnings = Vec::new();
        let scope_resolution = if params.scope.is_some() || !context_hints.is_empty() {
            let resolution = self.resolve_scope(params.scope, &context_hints).await?;
            if !resolution.unresolved_scope {
                filter.scopes_any = Some(vec![resolution.scope.clone()]);
            } else if !context_hints.is_empty() {
                warnings.push(quality_warning(
                    "unresolved_scope",
                    "context_hints did not match a registered scope; recall is using all visible memories",
                ));
            }
            Some(resolution)
        } else {
            None
        };
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
        for result in outcome.results {
            let card = self.recall_card(result, reranker_blend_weight).await?;
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
            warnings,
            results,
        })
    }

    #[tool(description = "Read one full memory by id. Trusted principals record a meaningful read activity event; anonymous public reads do not.")]
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
        let item = self.full_read_item(id, mem, use_outcome.recorded > 0).await?;
        let Some(memory) = item.memory else {
            return Err(rmcp::ErrorData::internal_error("found read item omitted memory".to_owned(), None));
        };
        success_json(&ReadResponse {
            operation: operation_summary(OperationStatus::NoOp, 0, Vec::new(), NextAction::None),
            memory,
            summary: item.summary,
            scope: item.scope,
            agent_label: item.agent_label,
            created_by_principal: item.created_by_principal,
            quality_flags: item.quality_flags,
            unresolved_scope: item.unresolved_scope,
            activity_recorded: item.activity_recorded,
        })
    }

    #[tool(
        description = "Read multiple full memories by id. Preserves input order, returns per-item not_found for missing/unreadable IDs, caps at max_batch_size, and records one read activity event for found IDs only for trusted principals."
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

        let mut found = Vec::new();
        let mut items = Vec::with_capacity(params.ids.len());
        for (index, id) in params.ids.into_iter().enumerate() {
            match self.engine.get_memory(&id, request_principal.as_deref()).await? {
                Some(mem) => {
                    found.push((index, id, mem));
                }
                None => items.push((index, ReadManyItemResponse {
                    id,
                    status: ReadManyStatus::NotFound,
                    memory: None,
                    summary: None,
                    scope: None,
                    agent_label: None,
                    created_by_principal: None,
                    quality_flags: Vec::new(),
                    unresolved_scope: false,
                    activity_recorded: false,
                })),
            }
        }

        let found_ids = found.iter().map(|(_index, id, _mem)| *id).collect::<Vec<_>>();
        let activity_recorded = if let Some(principal) = request_principal.as_deref() {
            self.engine
                .record_memory_use(found_ids.clone(), principal, READ_EVENT_WEIGHT)
                .await
                .unwrap_or_default()
                .recorded
                > 0
        } else {
            false
        };
        for (index, id, mem) in found {
            items.push((index, self.full_read_item(id, mem, activity_recorded).await?));
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
        let context_hints = normalize_optional_string_array("context_hints", Some(params.context_hints))
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let mut scope_resolution = None;
        let scope_update = if params.scope.is_some() || !context_hints.is_empty() {
            let resolution = self.resolve_scope(params.scope, &context_hints).await?;
            let scope = (!resolution.unresolved_scope).then_some(resolution.scope.clone());
            scope_resolution = Some(resolution);
            scope
        } else {
            None
        };
        let tags = normalize_optional_string_array("tags", params.tags).map_err(EngineError::from)?;
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
            entities: normalize_optional_entity_inputs(params.entities).map_err(EngineError::from)?,
        };
        let update_outcome = self.engine.update_memory_with_metadata(id, update, metadata_patch, &principal).await?;
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
            WriteOutcome::Applied => success_json(&UpdateResponse {
                operation: operation_summary(OperationStatus::Applied, 1, Vec::new(), NextAction::None),
                updated: true,
                scope_resolution,
            }),
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
        let context_hints = normalize_optional_string_array("context_hints", Some(params.context_hints))
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let mut filter = MemoryFilter::default();
        let scope_requested = params.scope.is_some() || !context_hints.is_empty();
        let mut warnings = Vec::new();
        let scope_resolution = if scope_requested {
            let resolution = self.resolve_scope(params.scope, &context_hints).await?;
            if !resolution.unresolved_scope {
                filter.scopes_any = Some(vec![resolution.scope.clone()]);
            } else if !context_hints.is_empty() {
                warnings.push(quality_warning(
                    "unresolved_scope",
                    "context_hints did not match a registered scope; brief is using all visible memories",
                ));
            }
            Some(resolution)
        } else {
            None
        };
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
        for result in outcome.results {
            let card = self.recall_card(result, reranker_blend_weight).await?;
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
        for candidate in params.candidates.into_iter().map(HandoffCandidate::from) {
            let HandoffCandidate {
                content,
                summary,
                scope,
                context_hints,
                tags,
                entities,
                memory_type,
            } = candidate;
            let metadata_summary = trim_optional_text(summary.clone());
            let context_hints = normalize_optional_string_array("context_hints", Some(context_hints))
                .map_err(EngineError::from)?
                .unwrap_or_default();
            let scope_resolution = self.resolve_scope(scope.clone(), &context_hints).await?;
            let resolved_scope = scope_resolution.scope.clone();
            let unresolved_scope = scope_resolution.unresolved_scope;
            let raw_entities_len = entities.len();
            let commit_payload = if commit {
                let memory_input = params::MemoryInput {
                    content: content.clone(),
                    tags: tags.clone(),
                    source_agent: write_principal.clone(),
                    source_conversation: Some(resolved_scope.clone()),
                    origin_conversation: Some(resolved_scope.clone()),
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
                let input = StoreMemoryInput::try_from(memory_input).map_err(EngineError::from)?;
                let supersedes = input.supersedes;
                Some((self.engine.build_memory(input, now)?, supersedes))
            } else {
                None
            };
            let warning_content = commit_payload.as_ref().map_or(content.as_str(), |(memory, _)| memory.content.as_str());
            let warning_tags = commit_payload.as_ref().map_or(tags.as_slice(), |(memory, _)| memory.tags.as_slice());
            let warning_entities_len = commit_payload.as_ref().map_or(raw_entities_len, |(memory, _)| memory.entities.len());
            let mut warnings = write_quality_warnings(warning_content, summary.as_deref(), unresolved_scope, warning_tags, warning_entities_len);
            let duplicate_principal = write_principal.as_deref().or(request_principal.as_deref());
            let duplicate_candidates = self.duplicate_candidates(warning_content, &resolved_scope, duplicate_principal).await.unwrap_or_default();
            if !duplicate_candidates.is_empty() {
                warnings.push(quality_warning(
                    "duplicate_candidate",
                    "similar memories already exist; review existing memories before committing this handoff candidate",
                ));
            }
            let quality_flags = warnings.iter().map(|warning| warning.code.clone()).collect();
            let next_action = next_action_for_warnings(&warnings);
            all_warnings.extend(warnings.clone());
            let write = commit_payload.map(|(memory, supersedes)| PreparedHandoffWrite {
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
            });
            prepared.push(PreparedHandoff {
                suggestion: HandoffSuggestion {
                    content,
                    scope: resolved_scope,
                    unresolved_scope,
                    scope_resolution,
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
            let mut suggestion_indexes = Vec::with_capacity(prepared.len());

            for (index, item) in prepared.iter_mut().enumerate() {
                if let Some(write) = item.write.take() {
                    suggestion_indexes.push(index);
                    supersedes.push(write.supersedes);
                    memories.push(write.memory);
                    metadata.push(write.metadata);
                }
            }

            let ids = self.engine.batch_store_with_metadata(memories, supersedes, metadata).await?;
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
        if self.write_principal_for(request_principal.as_deref()).is_none() {
            return Ok(Self::anonymous_write_denied());
        }
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
        self.engine.register_scope(scope.clone()).await?;
        success_json(&AdminScopeRegisterResponse { scope: ScopeEntry::from(scope) })
    }

    #[tool(description = "Read-like admin: list persisted scope definitions using read authorization from the server-resolved principal.")]
    async fn admin_scope_list(&self, context: RequestContext<RoleServer>, Parameters(_params): Parameters<AdminScopeListParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let scopes = self.engine.list_scopes().await?.into_iter().map(ScopeEntry::from).collect();
        success_json(&AdminScopeListResponse { scopes })
    }

    #[tool(description = "Read-like admin: list compact inventory cards for memories visible to the server-resolved principal without returning full content.")]
    async fn admin_list(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminListParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        if !self.read_allowed_for(request_principal.as_deref()) {
            return Ok(Self::anonymous_read_denied());
        }
        let common = self.common_filter_from_admin(params.filter, request_principal.as_deref()).await?;
        let (mut filter, ctx) = validate_and_normalize_filter(&common)?;
        filter.text_search = normalize_text_search(params.text_search)?;
        filter.has_embedding = params.has_embedding;
        filter.limit = params.limit;
        maybe_expand_scope_hierarchy(&mut filter, params.expand_scopes.unwrap_or(true));

        let now = self.engine.now();
        let memories = self.engine.list_memories(filter, ctx).await?;
        let mut cards = Vec::with_capacity(memories.len());
        for memory in memories {
            let card = self.inventory_card(memory, now).await?;
            cards.push(card);
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
        let registered_scope_keys = self.engine.list_scopes().await?.into_iter().map(|scope| scope.scope_key).collect::<Vec<_>>();
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
        reject_removed_admin_field(params.deprecated_origin_conversation.is_some(), "origin_conversation", "origin_scope")?;
        let origin_scope = normalize_optional_non_empty("origin_scope", params.origin_scope).map_err(EngineError::from)?;

        let reassigned = self.engine.reassign_scope(&from_scope, &to_scope, origin_scope.as_deref(), &principal).await?;
        success_json(&ReassignScopeResponse {
            operation: operation_summary(OperationStatus::Applied, reassigned, Vec::new(), NextAction::None),
            reassigned,
        })
    }

    #[tool(description = "Write/admin: destructively evict all expired TTL memories using the server-resolved principal.")]
    async fn admin_cleanup_expired(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(_params): Parameters<AdminCleanupExpiredParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        let deleted = self.engine.evict_expired(&principal).await?;
        tracing::info!(principal = principal.as_str(), deleted, "admin_cleanup_expired completed");
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
        let common = self.common_filter_from_admin(params.filter, request_principal.as_deref()).await?;
        let (mut filter, ctx) = validate_and_normalize_filter(&common)?;
        maybe_expand_scope_hierarchy(&mut filter, params.expand_scopes.unwrap_or(true));
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
        let common = self.common_filter_from_admin(params.filter, Some(&principal)).await?;
        let (mut filter, mut ctx) = validate_and_normalize_filter(&common)?;
        ctx.principal = Some(principal.clone());
        filter.text_search = normalize_text_search(params.text_search)?;
        maybe_expand_scope_hierarchy(&mut filter, params.expand_scopes.unwrap_or(true));

        let result = self.engine.bulk_delete(&principal, filter, ctx).await?;
        let mut operation = operation_summary(OperationStatus::Applied, result.deleted, Vec::new(), NextAction::None);
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
        let common = self.common_filter_from_admin(params.filter, Some(&principal)).await?;
        let (mut filter, mut ctx) = validate_and_normalize_filter(&common)?;
        ctx.principal = Some(principal.clone());
        filter.text_search = normalize_text_search(params.text_search)?;
        maybe_expand_scope_hierarchy(&mut filter, params.expand_scopes.unwrap_or(true));

        let set_tags = normalize_optional_string_array("set_tags", params.set_tags).map_err(EngineError::from)?;
        let fields = BulkUpdateFields {
            tags: set_tags,
            importance: params.importance.map(crate::types::Importance::new),
            access_policy: normalize_optional_access_policy(params.access_policy),
        };

        let result = self.engine.bulk_update(&principal, filter, ctx, fields).await?;
        let mut operation = operation_summary(OperationStatus::Applied, result.updated, Vec::new(), NextAction::None);
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

    #[tool(description = "Write/admin: find near-duplicate memories using the server-resolved principal. dry_run=true previews; dry_run=false merges by superseding duplicates.")]
    async fn admin_consolidate(&self, context: RequestContext<RoleServer>, Parameters(params): Parameters<AdminConsolidateParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let request_principal = self.principal_for_context(&context);
        let Some(principal) = self.write_principal_for(request_principal.as_deref()) else {
            return Ok(Self::anonymous_write_denied());
        };
        reject_removed_admin_field(params.deprecated_scope_keys_any.is_some(), "scope_keys_any", "scopes")?;
        let scopes_any = self.resolve_admin_scope_filter(params.scope, params.scopes).await?;

        let result = self
            .engine
            .consolidate_memories(&principal, scopes_any.as_deref(), params.similarity_threshold, params.limit, params.dry_run)
            .await?;

        let response = ConsolidateResponse {
            operation: operation_summary(
                if params.dry_run { OperationStatus::Preview } else { OperationStatus::Applied },
                u64::from(!params.dry_run && result.merged),
                Vec::new(),
                if params.dry_run { NextAction::Continue } else { NextAction::None },
            ),
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

    #[tool(description = "Write/admin: trigger re-embedding for one memory or a capped batch of unembedded memories using the server-resolved principal.")]
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
        let request = params.id.map_or(ReembedRequest::Bulk { limit: bulk_limit }, |id| ReembedRequest::Single { id, principal });

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
            " Privileged admin tools are enabled for migration, repair, statistics, re-embedding, consolidation, scope registry management, and audit history."
        } else {
            " Privileged admin tools are disabled by default and require explicit operator configuration."
        };
        info.instructions = Some(format!(
            "LocalHold is a deterministic local memory server. For normal agent work, start with \
             brief, use recall to get compact relevant cards, read or read_many to fetch full memory \
             content and record activity, and remember to store durable new information. Use handoff to validate \
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
