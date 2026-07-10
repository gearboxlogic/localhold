use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{AccessPolicy, Entity, Memory, MemoryId, MemoryType, ScopeDefinition, SearchMode, V2MetadataMigrationReport, V2MigrationReport};

// -- Default‐value functions for serde(default = "...") --

const fn default_recall_limit() -> usize {
    8
}

#[expect(clippy::unnecessary_wraps, reason = "serde default fn must match the field type Option<usize>")]
const fn default_list_limit() -> Option<usize> {
    Some(20)
}

const fn default_top_tags_limit() -> usize {
    10
}

#[expect(clippy::unnecessary_wraps, reason = "serde default fn must match the field type Option<bool>")]
const fn default_expand_scopes() -> Option<bool> {
    Some(true)
}

/// Merge the keys from a JSON Schema value into an existing property object.
/// Used by [`strip_nullable`] to inline the non-null branch of an `anyOf`.
fn merge_schema_into(prop: &mut serde_json::Map<String, serde_json::Value>, source: serde_json::Value) {
    if let serde_json::Value::Object(inner) = source {
        for (k, v) in inner {
            let _ = prop.entry(&k).or_insert(v);
        }
    }
}

/// Strip nullable wrappers from all direct properties of a JSON Schema object.
///
/// Gemini / Vertex AI function calling rejects schemas where `anyOf` has sibling
/// fields (`description`, `default`, `items`, etc.).  schemars 1.x produces two
/// patterns that trigger this:
///
/// 1. `Option<Vec<T>>`  → `type: ["array","null"]` + `items` + `default:null`
/// 2. `Option<EnumType>` → `anyOf: [{$ref:…},{type:"null"}]` + `description` + `default:null`
///
/// This transform converts every nullable property to its non-nullable equivalent.
/// Optionality is already expressed by the field's absence from `required`.
fn strip_nullable(schema: &mut schemars::Schema) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    let Some(serde_json::Value::Object(properties)) = obj.get_mut("properties") else {
        return;
    };

    for prop_value in properties.values_mut() {
        let Some(prop) = prop_value.as_object_mut() else {
            continue;
        };

        // Pattern 1: type: ["T", "null"] → type: "T"
        if let Some(serde_json::Value::Array(types)) = prop.get_mut("type") {
            types.retain(|t| t.as_str() != Some("null"));
            if types.len() == 1 {
                let single = types.remove(0);
                drop(prop.insert("type".into(), single));
            }
        }

        // Pattern 2: anyOf: [{schema}, {type:"null"}] → inline the non-null variant
        if let Some(serde_json::Value::Array(any_of)) = prop.remove("anyOf") {
            let mut non_null: Vec<_> = any_of
                .into_iter()
                .filter(|v| v.as_object().and_then(|o| o.get("type")).and_then(serde_json::Value::as_str) != Some("null"))
                .collect();
            if non_null.len() == 1 {
                // Merge the inner schema's keys into the property object.
                merge_schema_into(prop, non_null.remove(0));
            } else if !non_null.is_empty() {
                // More than one non-null branch — keep as anyOf (shouldn't happen
                // for simple Option<T>, but be defensive).
                drop(prop.insert("anyOf".into(), serde_json::Value::Array(non_null)));
            }
        }

        // Remove default: null — it's semantically wrong for a non-nullable schema
        // and some validators (including Gemini) may reject it.
        if prop.get("default") == Some(&serde_json::Value::Null) {
            drop(prop.remove("default"));
        }
    }
}

/// Request-only entity payload used by store/update APIs.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct EntityInput {
    /// The entity name (e.g., "Alice", "example-agent", "RFC 9110").
    pub name: String,
    /// The entity type (e.g., "person", "project", "document").
    #[serde(rename = "type")]
    pub entity_type: String,
}

impl JsonSchema for EntityInput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Entity")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A caller-provided typed entity attached to a memory for entity-based retrieval.",
            "type": "object",
            "properties": {
                "name": {
                    "description": "The entity name (e.g., \"Alice\", \"example-agent\", \"RFC 9110\").",
                    "type": "string"
                },
                "type": {
                    "description": "The entity type (e.g., \"person\", \"project\", \"document\").",
                    "type": "string"
                }
            },
            "required": ["name", "type"]
        })
    }
}

/// One request entity accepted by v2 write/update tools.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum EntityInputItem {
    /// String shorthand for an entity name; the entity type defaults to `unknown`.
    Name(String),
    /// Full object with explicit `name` and `type`.
    Structured(Box<EntityInput>),
}

impl From<EntityInputItem> for EntityInput {
    fn from(item: EntityInputItem) -> Self {
        match item {
            EntityInputItem::Name(name) => Self {
                name,
                entity_type: "unknown".to_owned(),
            },
            EntityInputItem::Structured(entity) => *entity,
        }
    }
}

impl From<Entity> for EntityInput {
    fn from(entity: Entity) -> Self {
        Self {
            name: entity.name,
            entity_type: entity.entity_type.to_string(),
        }
    }
}

/// String shorthand accepted by request access policy fields.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccessPolicyShorthand {
    /// Visible to all callers.
    Public,
}

/// One request access policy accepted by v2 write/update tools.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum AccessPolicyInput {
    /// String shorthand; currently only `"public"` is accepted.
    Shorthand(AccessPolicyShorthand),
    /// Full object for `public`, `restricted`, or `redacted` access.
    Structured(Box<AccessPolicy>),
}

impl From<AccessPolicyInput> for AccessPolicy {
    fn from(input: AccessPolicyInput) -> Self {
        match input {
            AccessPolicyInput::Shorthand(AccessPolicyShorthand::Public) => Self::Public,
            AccessPolicyInput::Structured(policy) => *policy,
        }
    }
}

/// Internal memory-store fields assembled from v2 tool inputs before engine validation.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub(crate) struct MemoryInput {
    /// The content to store as a memory.
    pub content: String,
    /// Tags for categorizing this memory.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Agent that created this memory.
    #[serde(default)]
    pub source_agent: Option<String>,
    /// Conversation context for this memory.
    #[serde(default)]
    pub source_conversation: Option<String>,
    /// Original conversation for this memory. Defaults to `source_conversation` when omitted.
    #[serde(default)]
    pub origin_conversation: Option<String>,
    /// User that triggered this memory.
    #[serde(default)]
    pub source_user: Option<String>,
    /// Time-to-live in seconds. After this duration, the memory may be evicted.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    /// Access policy: `"public"` shorthand, or a full object like `{"type": "restricted", "allowed": ["agent-1"]}`.
    #[serde(default)]
    pub access_policy: Option<AccessPolicyInput>,
    /// Memory type classification: `"semantic"` (default), `"episodic"`, or `"procedural"`.
    #[serde(default)]
    pub memory_type: Option<MemoryType>,
    /// Importance score from 0.0 (low) to 1.0 (high). Defaults to 0.5. Clamped to [0.0, 1.0].
    #[serde(default)]
    pub importance: Option<f64>,
    /// Confidence score from 0.0 (low) to 1.0 (high). Defaults to 0.8. Clamped to [0.0, 1.0].
    #[serde(default)]
    pub confidence: Option<f64>,
    /// ID of the memory this new memory supersedes (soft versioning).
    /// When set, the referenced memory's `superseded_by` field is set to this new memory's ID.
    #[serde(default)]
    pub supersedes: Option<MemoryId>,
    /// Typed entities to attach to this memory for entity-based retrieval.
    #[serde(default)]
    pub entities: Vec<EntityInputItem>,
}

/// Parameters for the v2 `remember` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct RememberParams {
    /// The durable memory content to store.
    pub content: String,
    /// Optional caller-supplied compact summary. The server stores the original content unchanged.
    #[serde(default)]
    pub summary: Option<String>,
    /// Scope key, alias, or value containing a registered matcher. If omitted and `context_hints` do not resolve, writes to `inbox/unresolved`.
    #[serde(default)]
    pub scope: Option<String>,
    /// Path, git, or workflow hints matched against registered scope matchers when `scope` is omitted.
    #[serde(default)]
    pub context_hints: Vec<String>,
    /// Human-readable agent label for provenance. This does not grant access.
    #[serde(default)]
    pub agent_label: Option<String>,
    /// Tags for categorizing this memory.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Entities to attach: each item may be a string name shorthand or a full `{name, type}` object.
    #[serde(default)]
    pub entities: Vec<EntityInputItem>,
    /// Memory type classification.
    #[serde(default)]
    pub memory_type: Option<MemoryType>,
    /// Importance score from 0.0 (low) to 1.0 (high).
    #[serde(default)]
    pub importance: Option<f64>,
    /// Confidence score from 0.0 (low) to 1.0 (high).
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Access policy: `"public"` shorthand or a full policy object.
    #[serde(default)]
    pub access_policy: Option<AccessPolicyInput>,
}

/// Parameters for the v2 `recall` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct RecallParams {
    /// The search query text.
    pub query: String,
    /// Maximum number of compact cards to return.
    #[serde(default = "default_recall_limit")]
    pub limit: usize,
    /// Optional scope filter: registered scope key, alias, or value containing a registered matcher.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional context hints matched against registered scope matchers when `scope` is omitted.
    #[serde(default)]
    pub context_hints: Vec<String>,
    /// Optional tags filter.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Optional entity-name filter.
    #[serde(default)]
    pub entity: Option<String>,
    /// Include weak matches in the main result list.
    #[serde(default)]
    pub include_weak: bool,
    /// Optional explicit search mode.
    #[serde(default)]
    pub search_mode: Option<SearchMode>,
    /// Optional exact identifiers or literal terms for keyword matching.
    #[serde(default)]
    pub literal_terms: Option<String>,
    /// Optional extra task/query context for semantic and keyword retrieval.
    #[serde(default)]
    pub query_context: Option<String>,
}

/// One item accepted by the v2 `remember_many` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum RememberManyItem {
    /// String shorthand for a durable memory's content.
    Content(String),
    /// Full structured `remember` parameters.
    Structured(Box<RememberParams>),
}

impl From<RememberManyItem> for RememberParams {
    fn from(item: RememberManyItem) -> Self {
        match item {
            RememberManyItem::Content(content) => Self {
                content,
                summary: None,
                scope: None,
                context_hints: Vec::new(),
                agent_label: None,
                tags: Vec::new(),
                entities: Vec::new(),
                memory_type: None,
                importance: None,
                confidence: None,
                access_policy: None,
            },
            RememberManyItem::Structured(params) => *params,
        }
    }
}

/// Parameters for the v2 `remember_many` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RememberManyParams {
    /// Memories to validate and store atomically. Each item may be a string content shorthand or a full `remember` object. Length is capped by `limits.max_batch_size`.
    pub memories: Vec<RememberManyItem>,
}

/// Parameters for the v2 `read` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct ReadParams {
    /// The memory ID to retrieve.
    pub id: MemoryId,
}

/// Parameters for the v2 `read_many` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct ReadManyParams {
    /// Memory IDs to retrieve. Order is preserved; missing or unreadable IDs return per-item `not_found`; batch size is capped by `limits.max_batch_size`.
    pub ids: Vec<MemoryId>,
}

/// Parameters for the v2 `revise` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct ReviseParams {
    /// The memory ID to revise.
    pub id: MemoryId,
    /// Replacement content. When set, embeddings are regenerated in the background.
    #[serde(default)]
    pub content: Option<String>,
    /// Replacement compact summary for v2 cards.
    #[serde(default)]
    pub summary: Option<String>,
    /// Replacement human-readable agent label. This does not grant access.
    #[serde(default)]
    pub agent_label: Option<String>,
    /// Replacement scope: registered scope key, alias, or value containing a registered matcher.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional context hints matched against registered scope matchers when `scope` is omitted.
    #[serde(default)]
    pub context_hints: Vec<String>,
    /// Replacement tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Replacement access policy: `"public"` shorthand or a full policy object.
    #[serde(default)]
    pub access_policy: Option<AccessPolicyInput>,
    /// Replacement importance score.
    #[serde(default)]
    pub importance: Option<f64>,
    /// Replacement confidence score.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Replacement entities: each item may be a string name shorthand or a full `{name, type}` object.
    #[serde(default)]
    pub entities: Option<Vec<EntityInputItem>>,
}

/// Parameters for the v2 `forget` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ForgetParams {
    /// The memory ID to delete.
    pub id: MemoryId,
}

/// Parameters for the v2 `brief` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct BriefParams {
    /// Optional topic or task query.
    #[serde(default)]
    pub query: Option<String>,
    /// Optional scope filter: registered scope key, alias, or value containing a registered matcher.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional context hints matched against registered scope matchers when `scope` is omitted.
    #[serde(default)]
    pub context_hints: Vec<String>,
    /// Maximum number of relevant cards to include.
    #[serde(default = "default_recall_limit")]
    pub limit: usize,
}

/// Candidate memory supplied to `handoff`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct HandoffCandidate {
    /// Candidate memory content.
    pub content: String,
    /// Optional compact summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// Optional target scope: registered scope key, alias, or value containing a registered matcher.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional context hints matched against registered scope matchers when `scope` is omitted.
    #[serde(default)]
    pub context_hints: Vec<String>,
    /// Candidate tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Candidate entities: each item may be a string name shorthand or a full `{name, type}` object.
    #[serde(default)]
    pub entities: Vec<EntityInputItem>,
    /// Candidate memory type.
    #[serde(default)]
    pub memory_type: Option<MemoryType>,
}

/// One candidate item accepted by the v2 `handoff` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
#[non_exhaustive]
pub enum HandoffCandidateItem {
    /// String shorthand for candidate memory content.
    Content(String),
    /// Full structured handoff candidate.
    Structured(Box<HandoffCandidate>),
}

impl From<HandoffCandidateItem> for HandoffCandidate {
    fn from(item: HandoffCandidateItem) -> Self {
        match item {
            HandoffCandidateItem::Content(content) => Self {
                content,
                summary: None,
                scope: None,
                context_hints: Vec::new(),
                tags: Vec::new(),
                entities: Vec::new(),
                memory_type: None,
            },
            HandoffCandidateItem::Structured(candidate) => *candidate,
        }
    }
}

/// Parameters for the v2 `handoff` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct HandoffParams {
    /// Candidate memories to validate. Each item may be a string content shorthand or a full candidate object. Length is capped by `limits.max_batch_size`.
    pub candidates: Vec<HandoffCandidateItem>,
    /// Persist validated candidates when true. Defaults to preview-only.
    #[serde(default)]
    pub commit: bool,
}

/// Register or replace a scope definition.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminScopeRegisterParams {
    /// Stable scope key, e.g. `gearboxlogic/localhold`.
    pub scope_key: String,
    /// Display name for humans and agents.
    pub display_name: String,
    /// Optional scope description.
    #[serde(default)]
    pub description: Option<String>,
    /// Alternate aliases that resolve to this scope key.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Substrings matched against explicit scope values or `context_hints` to resolve this scope key.
    #[serde(default)]
    pub matchers: Vec<String>,
    /// Optional parent scope key.
    #[serde(default)]
    pub parent: Option<String>,
    /// Optional related scope keys.
    #[serde(default)]
    pub related: Vec<String>,
}

/// Parameters for listing registered scopes.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
#[expect(clippy::empty_structs_with_brackets, reason = "MCP params schema must remain an empty JSON object")]
pub struct AdminScopeListParams {}

/// Parameters for reporting v2 metadata migration state.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
#[expect(clippy::empty_structs_with_brackets, reason = "MCP params schema must remain an empty JSON object")]
pub struct AdminV2MigrationReportParams {}

/// Parameters for applying non-destructive v2 metadata migration.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AdminV2MigrateMetadataParams {
    /// When true, report what would be migrated without inserting metadata rows.
    #[serde(default)]
    pub dry_run: bool,
}

/// Shared filter fields for v2 admin tools. Authorization identity is resolved by the server.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
#[expect(
    clippy::partial_pub_fields,
    clippy::field_scoped_visibility_modifiers,
    reason = "hidden deprecated serde aliases are handler-only traps and are skipped from MCP schemas"
)]
pub struct AdminFilterFields {
    /// Filter to memories with any of these tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Filter to memories with this agent provenance label.
    #[serde(default)]
    pub agent_label: Option<String>,
    /// Filter by scope key, alias, or value containing a registered matcher.
    #[serde(default)]
    pub scope: Option<String>,
    /// Filter by any scope key, alias, or matcher-containing value.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// Filter to memories whose historical origin scope matches this value.
    #[serde(default)]
    pub origin_scope: Option<String>,
    /// Filter to memories of this type: `"semantic"`, `"episodic"`, or `"procedural"`.
    #[serde(default)]
    pub memory_type: Option<MemoryType>,
    /// When `true`, include memories that have been superseded by newer versions.
    /// Default is `false` — superseded memories are hidden.
    #[serde(default)]
    pub include_superseded: Option<bool>,
    /// Filter to memories tagged with this entity name.
    #[serde(default)]
    pub entity: Option<String>,
    /// Filter to memories tagged with entities of this type.
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default, rename = "source_agent")]
    #[schemars(skip)]
    pub(crate) deprecated_source_agent: Option<serde_json::Value>,
    #[serde(default, rename = "source_conversation")]
    #[schemars(skip)]
    pub(crate) deprecated_source_conversation: Option<serde_json::Value>,
    #[serde(default, rename = "origin_conversation")]
    #[schemars(skip)]
    pub(crate) deprecated_origin_conversation: Option<serde_json::Value>,
    #[serde(default, rename = "scope_keys_any")]
    #[schemars(skip)]
    pub(crate) deprecated_scope_keys_any: Option<serde_json::Value>,
}

/// Parameters for the v2 admin scope reassignment tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
#[expect(
    clippy::partial_pub_fields,
    clippy::field_scoped_visibility_modifiers,
    reason = "hidden deprecated serde alias is a handler-only trap and is skipped from MCP schemas"
)]
pub struct AdminReassignScopeParams {
    /// Current scope key to move from.
    pub from_scope: String,
    /// New scope key to move into.
    pub to_scope: String,
    /// Optional historical origin scope filter; when set, only matching rows are moved.
    #[serde(default)]
    pub origin_scope: Option<String>,
    #[serde(default, rename = "origin_conversation")]
    #[schemars(skip)]
    pub(crate) deprecated_origin_conversation: Option<serde_json::Value>,
}

/// Parameters for v2 admin expired-memory cleanup.
#[derive(Debug, Deserialize, JsonSchema)]
#[non_exhaustive]
#[expect(clippy::empty_structs_with_brackets, reason = "MCP params schema must remain an empty JSON object")]
pub struct AdminCleanupExpiredParams {}

/// Parameters for v2 admin aggregate memory counts.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminCountParams {
    /// Filter fields to constrain the counted memories.
    #[serde(flatten)]
    pub filter: AdminFilterFields,
    /// How many top tags to return in the breakdown (default 10).
    #[serde(default = "default_top_tags_limit")]
    pub top_tags_limit: usize,
    /// When `true` (the default), scope keys are expanded to include
    /// all ancestor scopes (e.g., `"org/project/conv"` also matches `"org/project"` and `"org"`).
    /// Set to `false` to disable hierarchical scoping and match only the exact scope keys provided.
    #[serde(default = "default_expand_scopes")]
    pub expand_scopes: Option<bool>,
}

/// Parameters for v2 admin bulk deletion.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminBulkDeleteParams {
    /// Filter fields to identify which memories to delete.
    #[serde(flatten)]
    pub filter: AdminFilterFields,
    /// Additional content filter applied to memory text.
    #[serde(default)]
    pub text_search: Option<String>,
    /// When `true` (the default), scope keys are expanded to include
    /// all ancestor scopes (e.g., `"org/project/conv"` also matches `"org/project"` and `"org"`).
    /// Set to `false` to disable hierarchical scoping and match only the exact scope keys provided.
    #[serde(default = "default_expand_scopes")]
    pub expand_scopes: Option<bool>,
}

/// Parameters for v2 admin bulk metadata updates.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminBulkUpdateParams {
    /// Filter fields to identify which memories to update.
    #[serde(flatten)]
    pub filter: AdminFilterFields,
    /// Additional content filter applied to memory text.
    #[serde(default)]
    pub text_search: Option<String>,
    /// New tags to replace existing tags on matching memories.
    #[serde(default)]
    pub set_tags: Option<Vec<String>>,
    /// New importance score in [0.0, 1.0]. Clamped to range.
    #[serde(default)]
    pub importance: Option<f64>,
    /// New access policy to replace existing access policy.
    #[serde(default)]
    pub access_policy: Option<AccessPolicyInput>,
    /// When `true` (the default), scope keys are expanded to include
    /// all ancestor scopes (e.g., `"org/project/conv"` also matches `"org/project"` and `"org"`).
    /// Set to `false` to disable hierarchical scoping and match only the exact scope keys provided.
    #[serde(default = "default_expand_scopes")]
    pub expand_scopes: Option<bool>,
}

/// Parameters for v2 admin duplicate consolidation.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
#[expect(
    clippy::partial_pub_fields,
    clippy::field_scoped_visibility_modifiers,
    reason = "hidden deprecated serde alias is a handler-only trap and is skipped from MCP schemas"
)]
pub struct AdminConsolidateParams {
    /// Optional scope filter: registered scope key, alias, or value containing a registered matcher.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional any-match scope filter: scope keys, aliases, or matcher-containing values.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default, rename = "scope_keys_any")]
    #[schemars(skip)]
    pub(crate) deprecated_scope_keys_any: Option<serde_json::Value>,
    /// Cosine similarity threshold for grouping (0.0–1.0). Default is 0.85.
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f64,
    /// Maximum number of duplicate groups to return. Default is 10.
    #[serde(default = "default_consolidate_limit")]
    pub limit: usize,
    /// When `true` (default), report duplicates without merging.
    /// When `false`, merge by superseding duplicate members.
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
}

/// Parameters for v2 admin audit history reads.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminHistoryParams {
    /// The memory ID to query audit history for.
    pub id: MemoryId,
    /// Maximum number of audit entries to return. Default is 50; clamped to the server maximum.
    #[serde(default = "default_history_limit")]
    pub limit: usize,
}

/// Parameters for v2 admin re-embedding.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminReembedParams {
    /// Specific memory ID to re-embed. When omitted, re-embeds all memories without embeddings.
    #[serde(default)]
    pub id: Option<MemoryId>,
    /// Maximum number of memories to re-embed in bulk mode (when `id` is omitted). Defaults to a server-configured limit.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Parameters for the v2 admin inventory listing tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub struct AdminListParams {
    /// Filter fields to constrain the inventory.
    #[serde(flatten)]
    pub filter: AdminFilterFields,
    /// Filter to memories containing this text.
    #[serde(default)]
    pub text_search: Option<String>,
    /// Filter to memories with (`true`) or without (`false`) embeddings.
    #[serde(default)]
    pub has_embedding: Option<bool>,
    /// Maximum number of inventory cards to return.
    #[serde(default = "default_list_limit")]
    pub limit: Option<usize>,
    /// Expand scope keys to include ancestor scopes.
    #[serde(default = "default_expand_scopes")]
    pub expand_scopes: Option<bool>,
}

/// Internal filter fields assembled from v2/admin tool inputs before engine validation.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(transform = strip_nullable)]
#[non_exhaustive]
pub(crate) struct CommonFilterFields {
    /// Filter to memories with any of these tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Filter to memories from this agent provenance label.
    #[serde(default)]
    pub agent_label: Option<String>,
    /// Filter to memories in this scope.
    #[serde(default)]
    pub scope: Option<String>,
    /// Filter to memories that originated in this scope.
    #[serde(default)]
    pub origin_scope: Option<String>,
    /// Filter to memories whose scope is any of these keys.
    #[serde(default)]
    pub scopes_any: Option<Vec<String>>,
    /// Trusted principal identity from the hosting MCP runtime. When absent, only public memories are returned.
    #[serde(default)]
    pub principal: Option<String>,
    /// Filter to memories of this type: `"semantic"`, `"episodic"`, or `"procedural"`.
    #[serde(default)]
    pub memory_type: Option<MemoryType>,
    /// When `true`, include memories that have been superseded by newer versions.
    /// Default is `false` — superseded memories are hidden.
    #[serde(default)]
    pub include_superseded: Option<bool>,
    /// Filter to memories tagged with this entity name.
    #[serde(default)]
    pub entity: Option<String>,
    /// Filter to memories tagged with entities of this type.
    #[serde(default)]
    pub entity_type: Option<String>,
}

// -- Response types for JSON serialization --

/// Stable code for structured v2 application-level tool errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolErrorCode {
    /// Input was syntactically valid JSON but failed tool validation.
    InvalidParams,
    /// Requested memory or resource was not found or was not visible to this principal.
    NotFound,
    /// Principal lacks the requested access.
    AccessDenied,
    /// Anonymous reads are disabled by server policy.
    AnonymousReadDenied,
    /// Anonymous writes are disabled by server policy.
    AnonymousWriteDenied,
    /// Required backend capability is unavailable.
    Unavailable,
    /// Request conflicts with current state.
    Conflict,
    /// Unexpected internal failure.
    Internal,
}

/// Structured v2 application-level tool error.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ToolError {
    /// Stable machine-readable error code.
    pub code: ToolErrorCode,
    /// Optional request field path associated with the error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Human-readable summary.
    pub message: String,
    /// Optional suggested agent action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
    /// Whether retrying the same request may succeed later.
    pub retryable: bool,
}

/// Response envelope returned as JSON text when a v2 tool sets `is_error=true`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ToolErrorResponse {
    /// Structured application-level error.
    pub error: ToolError,
}

/// Severity for v2 quality warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum QualityWarningSeverity {
    /// Informational hint; the operation is usually usable.
    Info,
    /// Warning worth reviewing before relying on the result.
    Warning,
    /// Follow-up action is required for best results.
    ActionRequired,
}

/// A quality or classification warning returned by v2 write tools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct QualityWarning {
    /// Stable warning code.
    pub code: String,
    /// Severity for agent triage.
    pub severity: QualityWarningSeverity,
    /// Optional request field path associated with this warning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Human-readable warning message.
    pub message: String,
    /// Optional suggested agent action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// Standard operation status for v2 mutation and admin tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationStatus {
    /// The operation changed persistent state.
    Applied,
    /// The operation changed some, but not all, matching items.
    Partial,
    /// The operation only previewed changes.
    Preview,
    /// Work was accepted for asynchronous processing.
    Queued,
    /// The operation was valid but made no changes.
    NoOp,
    /// The principal lacked permission.
    Denied,
    /// The target was not found.
    NotFound,
}

/// Recommended follow-up action for agents after a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NextAction {
    /// No follow-up is needed.
    None,
    /// Read the returned or affected memory.
    Read,
    /// Review duplicate candidates before relying on the write.
    ReviewDuplicates,
    /// Classify unresolved scope later.
    ClassifyScope,
    /// Re-run the same maintenance operation to continue a capped batch.
    Continue,
    /// Inspect warnings before proceeding.
    ReviewWarnings,
}

/// Compact affected item for action-oriented operation responses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AffectedMemory {
    /// Affected memory ID.
    pub id: MemoryId,
    /// Compact card when the memory still exists and is readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<RecallCard>,
}

/// Shared action-oriented operation metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct OperationSummary {
    /// Operation status.
    pub status: OperationStatus,
    /// Number of records changed.
    pub changed: u64,
    /// Number of records matched before access checks, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<u64>,
    /// Number of matching records denied by authorization, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied: Option<u64>,
    /// Whether more matching records remain.
    pub capped: bool,
    /// Recommended next action for agents.
    pub next_action: NextAction,
    /// Warnings for this operation.
    pub warnings: Vec<QualityWarning>,
    /// Affected memories or representative samples.
    pub affected: Vec<AffectedMemory>,
}

/// A compact duplicate candidate card.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct DuplicateCandidateCard {
    /// Candidate memory ID.
    pub id: MemoryId,
    /// Compact excerpt.
    pub summary_or_excerpt: String,
    /// Agent-facing match assessment.
    pub r#match: MatchAssessment,
}

/// Response from v2 `remember`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RememberResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// The unique ID of the newly stored memory.
    pub id: MemoryId,
    /// Resolved scope used for the write.
    pub scope: String,
    /// Whether the scope is unresolved and should be classified later.
    pub unresolved_scope: bool,
    /// How the scope was resolved.
    pub scope_resolution: ScopeResolution,
    /// Potential duplicate memories.
    pub duplicate_candidates: Vec<DuplicateCandidateCard>,
    /// Quality warnings for the accepted write.
    pub warnings: Vec<QualityWarning>,
}

/// Per-item result from v2 `remember_many`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RememberManyItemResponse {
    /// Stored memory ID.
    pub id: MemoryId,
    /// Resolved scope used for the write.
    pub scope: String,
    /// Whether the scope is unresolved and should be classified later.
    pub unresolved_scope: bool,
    /// How the scope was resolved.
    pub scope_resolution: ScopeResolution,
    /// Potential duplicate memories.
    pub duplicate_candidates: Vec<DuplicateCandidateCard>,
    /// Quality warnings for the accepted write.
    pub warnings: Vec<QualityWarning>,
}

/// Response from v2 `remember_many`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RememberManyResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Per-memory write results.
    pub memories: Vec<RememberManyItemResponse>,
}

/// Agent-facing match quality label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchQuality {
    /// Strong match.
    Strong,
    /// Usable but not definitive.
    Possible,
    /// Weak match suppressed by default.
    Weak,
}

/// Recommended agent action for a matched card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchAction {
    /// Read the full memory.
    Read,
    /// Consider reading if more context is needed.
    Consider,
    /// Ignore unless explicitly debugging retrieval.
    Ignore,
}

/// Basis used to compute the agent-facing match score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchScoreBasis {
    /// Blended cross-encoder reranker score and first-stage retrieval score.
    RerankerBlend,
    /// First-stage retrieval score only.
    Retrieval,
    /// Low confidence association score for expanded/contextual candidates.
    Association,
    /// No ranking signal was available.
    Unavailable,
}

/// Agent-facing match assessment for a compact v2 card.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MatchAssessment {
    /// Match quality label.
    pub quality: MatchQuality,
    /// Recommended action for agents.
    pub action: MatchAction,
    /// Final query relevance on a 0.0-1.0 scale.
    pub score: f64,
    /// Ranking signal used to compute `score`.
    pub score_basis: MatchScoreBasis,
}

/// Ranking diagnostics for a compact v2 card.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MatchDiagnostics {
    /// First-stage retrieval score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_score: Option<f64>,
    /// Reranker score when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reranker_score: Option<f64>,
    /// Reranker blend weight used when `score_basis` is `reranker_blend`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reranker_blend_weight: Option<f64>,
    /// L2 vector distance when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f64>,
    /// Internal ranking score combining relevance, importance, freshness, activity, and confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking_score: Option<f64>,
}

/// Mechanism used to resolve a v2 scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScopeResolvedBy {
    /// Explicit scope key or raw explicit scope value.
    Explicit,
    /// Scope registry alias matched.
    Alias,
    /// Scope registry matcher matched an explicit scope or context hint.
    Matcher,
    /// No scope could be resolved.
    Unresolved,
}

/// Diagnostic details for agent-facing scope resolution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScopeResolution {
    /// Resolved scope key, or `inbox/unresolved`.
    pub scope: String,
    /// Whether the scope still needs classification.
    pub unresolved_scope: bool,
    /// Resolution mechanism.
    pub resolved_by: ScopeResolvedBy,
    /// Input hint or explicit value that matched, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_hint: Option<String>,
    /// Registry alias or matcher value that matched, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_value: Option<String>,
}

/// Compact v2 result card. Full content is available through `read`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RecallCard {
    /// Memory ID.
    pub id: MemoryId,
    /// Caller-supplied summary when available, otherwise a deterministic excerpt.
    pub summary_or_excerpt: String,
    /// Scope label/key.
    pub scope: String,
    /// Agent provenance label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Creation timestamp.
    pub created_at: String,
    /// Last-update timestamp.
    pub updated_at: String,
    /// Tags associated with the memory.
    pub tags: Vec<String>,
    /// Typed entities attached to the memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
    /// Agent-facing match assessment.
    pub r#match: MatchAssessment,
    /// Debugging diagnostics for retrieval and ranking.
    pub diagnostics: MatchDiagnostics,
}

/// Response from v2 `recall`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RecallResponse {
    /// Search strategy used.
    pub search_mode: SearchMode,
    /// Number of returned cards.
    pub count: usize,
    /// Number of weak matches suppressed from `results`.
    pub weak_result_count: usize,
    /// Scope-resolution diagnostics when a scope or context hints were supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_resolution: Option<ScopeResolution>,
    /// Deterministic warnings about scope or retrieval quality.
    pub warnings: Vec<QualityWarning>,
    /// Compact result cards.
    pub results: Vec<RecallCard>,
}

/// Response from v2 `read`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ReadResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Full memory content and metadata.
    pub memory: MemoryEntry,
    /// V2 compact summary when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Resolved v2 scope key when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Agent provenance label when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Principal that created v2 metadata when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_principal: Option<String>,
    /// Quality flags recorded with v2 metadata.
    pub quality_flags: Vec<String>,
    /// Whether this memory is still unresolved.
    pub unresolved_scope: bool,
    /// Whether activity tracking recorded this read.
    pub activity_recorded: bool,
}

/// Per-item status from v2 `read_many`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReadManyStatus {
    /// Memory was found and readable.
    Found,
    /// Memory was missing or not readable by this principal.
    NotFound,
}

/// Per-ID result from v2 `read_many`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ReadManyItemResponse {
    /// Requested memory ID.
    pub id: MemoryId,
    /// Per-item result status.
    pub status: ReadManyStatus,
    /// Full memory content and metadata when found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryEntry>,
    /// V2 compact summary when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Resolved v2 scope key when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Agent provenance label when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Principal that created v2 metadata when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_principal: Option<String>,
    /// Quality flags recorded with v2 metadata.
    pub quality_flags: Vec<String>,
    /// Whether this memory is still unresolved.
    pub unresolved_scope: bool,
    /// Whether activity tracking recorded this read.
    pub activity_recorded: bool,
}

/// Response from v2 `read_many`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ReadManyResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Per-ID results in input order.
    pub results: Vec<ReadManyItemResponse>,
}

/// Tool name for a deterministic next action recommended by `brief`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecommendedActionTool {
    /// Read one full memory by ID.
    Read,
    /// Read multiple full memories by ID.
    ReadMany,
    /// Run a focused recall search.
    Recall,
    /// Store a new durable memory.
    Remember,
    /// Register a missing scope definition.
    AdminScopeRegister,
}

/// Priority for a deterministic next action recommended by `brief`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecommendedActionPriority {
    /// Do this first.
    High,
    /// Useful but not urgent.
    Normal,
    /// Optional follow-up for weak or stale context.
    Low,
}

/// Deterministic next action recommended by `brief`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RecommendedAction {
    /// Tool to call next.
    pub tool: RecommendedActionTool,
    /// Action priority.
    pub priority: RecommendedActionPriority,
    /// Why this action is recommended.
    pub reason: String,
    /// Complete tool arguments when the server can construct a valid call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

/// Response from v2 `brief`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BriefResponse {
    /// Relevant memories for the task/topic.
    pub relevant: Vec<RecallCard>,
    /// Decision-like memories.
    pub decisions: Vec<RecallCard>,
    /// Work-in-progress memories.
    pub wip: Vec<RecallCard>,
    /// Lesson memories.
    pub lessons: Vec<RecallCard>,
    /// Stale or weak candidates.
    pub stale_candidates: Vec<RecallCard>,
    /// IDs worth reading for full content.
    pub suggested_reads: Vec<MemoryId>,
    /// Deterministic next tool calls an agent can take.
    pub recommended_actions: Vec<RecommendedAction>,
    /// Scope-resolution diagnostics when a scope or context hints were supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_resolution: Option<ScopeResolution>,
    /// Deterministic warnings about scope or empty context.
    pub warnings: Vec<QualityWarning>,
}

/// Suggested handoff write.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct HandoffSuggestion {
    /// Candidate content.
    pub content: String,
    /// Resolved target scope.
    pub scope: String,
    /// Whether the scope is unresolved.
    pub unresolved_scope: bool,
    /// How the scope was resolved.
    pub scope_resolution: ScopeResolution,
    /// Quality warnings.
    pub warnings: Vec<QualityWarning>,
    /// Stored memory ID when `commit=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<MemoryId>,
    /// Potential duplicate memories.
    pub duplicate_candidates: Vec<DuplicateCandidateCard>,
    /// Recommended next action for this candidate.
    pub next_action: NextAction,
}

/// Response from v2 `handoff`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct HandoffResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Whether candidates were persisted.
    pub committed: bool,
    /// Validated candidate writes.
    pub suggested_writes: Vec<HandoffSuggestion>,
}

/// Scope registry entry.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScopeEntry {
    /// Stable scope key.
    pub scope_key: String,
    /// Display name.
    pub display_name: String,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Aliases.
    pub aliases: Vec<String>,
    /// Matchers.
    pub matchers: Vec<String>,
    /// Optional parent scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Related scopes.
    pub related: Vec<String>,
}

/// Response from admin scope register.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AdminScopeRegisterResponse {
    /// Registered scope.
    pub scope: ScopeEntry,
}

/// Response from admin scope list.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AdminScopeListResponse {
    /// Registered scopes.
    pub scopes: Vec<ScopeEntry>,
}

/// Response from admin v2 migration report.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AdminV2MigrationReportResponse {
    /// Conservative migration/reporting counts.
    pub report: V2MigrationReport,
}

/// Response from admin v2 metadata migration.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AdminV2MigrateMetadataResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Whether this pass was a dry run.
    pub dry_run: bool,
    /// Non-destructive migration pass outcome.
    pub report: V2MetadataMigrationReport,
}

/// Compact inventory card for admin listing without query relevance.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[expect(clippy::struct_excessive_bools, reason = "inventory cards expose independent agent-facing state flags")]
pub struct InventoryCard {
    /// Memory ID.
    pub id: MemoryId,
    /// Caller-supplied summary when available, otherwise a deterministic excerpt.
    pub summary_or_excerpt: String,
    /// Scope label/key.
    pub scope: String,
    /// Agent provenance label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Creation timestamp.
    pub created_at: String,
    /// Last-update timestamp.
    pub updated_at: String,
    /// Tags associated with the memory.
    pub tags: Vec<String>,
    /// Typed entities attached to the memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
    /// Memory classification.
    pub memory_type: MemoryType,
    /// Whether the memory has an embedding.
    pub has_embedding: bool,
    /// Whether the memory is unresolved.
    pub unresolved_scope: bool,
    /// Whether the memory is expired.
    pub expired: bool,
    /// Whether this memory has been superseded.
    pub superseded: bool,
    /// V2 quality flags.
    pub quality_flags: Vec<String>,
}

/// Response from v2 `admin_list`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AdminListResponse {
    /// Number of inventory cards returned.
    pub count: usize,
    /// Compact inventory cards.
    pub memories: Vec<InventoryCard>,
    /// Deterministic warnings.
    pub warnings: Vec<QualityWarning>,
}

impl From<ScopeDefinition> for ScopeEntry {
    fn from(scope: ScopeDefinition) -> Self {
        Self {
            scope_key: scope.scope_key,
            display_name: scope.display_name,
            description: scope.description,
            aliases: scope.aliases,
            matchers: scope.matchers,
            parent: scope.parent,
            related: scope.related,
        }
    }
}

/// Common fields shared by full memory wire entries.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MemoryBase {
    /// The memory ID.
    pub id: MemoryId,
    /// The memory content.
    pub content: String,
    /// Tags associated with this memory.
    pub tags: Vec<String>,
    /// Timestamp when the memory was created.
    pub created_at: String,
    /// Timestamp when the memory's content was last modified.
    pub updated_at: String,
    /// Memory classification: `semantic`, `episodic`, or `procedural`.
    pub memory_type: MemoryType,
    /// Importance score in the range `[0.0, 1.0]`.
    pub importance: f64,
    /// Source trustworthiness in the range `[0.0, 1.0]`.
    pub confidence: f64,
    /// Whether this memory has a stored embedding vector.
    pub has_embedding: bool,
    /// Number of times this memory was shown in search results.
    pub impression_count: u64,
    /// ID of the memory that supersedes this one (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MemoryId>,
    /// Typed entities attached to this memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
}

/// A memory entry returned by list or get operations.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MemoryEntry {
    /// Common memory fields.
    #[serde(flatten)]
    pub base: MemoryBase,
    /// When this memory expires (RFC 3339), if a TTL was set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Timestamp of the last time this memory was shown in a search result (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_impressed_at: Option<String>,
}

impl std::ops::Deref for MemoryEntry {
    type Target = MemoryBase;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl std::ops::DerefMut for MemoryEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

/// Response from v2 `revise` and compatible admin update operations.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct UpdateResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Whether the memory was successfully updated.
    pub updated: bool,
    /// Scope-resolution diagnostics when a scope or context hints were supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_resolution: Option<ScopeResolution>,
}

/// Response from v2 `forget` and compatible admin delete operations.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct DeleteResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Whether the memory was successfully deleted.
    pub deleted: bool,
}

/// Response from v2 `admin_cleanup_expired`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct EvictExpiredResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Number of expired memories that were deleted.
    pub deleted: u64,
}

/// Response from v2 `admin_reassign_scope`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ReassignScopeResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Number of memories whose scope was reassigned.
    pub reassigned: u64,
}

// -- New feature params --

/// Response from v2 `admin_count`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct CountResponse {
    /// Total number of matching memories.
    pub total: u64,
    /// Count of memories that have an embedding vector.
    pub with_embedding: u64,
    /// Count of memories that lack an embedding vector.
    pub without_embedding: u64,
    /// Count of memories that have expired.
    pub expired: u64,
    /// Breakdown of memory counts by tag.
    pub by_tag: Vec<TagCount>,
    /// Breakdown of memory counts by agent label.
    pub by_agent_label: Vec<AgentCount>,
    /// Estimated database storage size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_bytes: Option<u64>,
    /// Creation timestamp of the oldest memory (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_memory: Option<String>,
    /// Creation timestamp of the newest memory (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_memory: Option<String>,
    /// Number of distinct scope keys.
    pub scope_count: u64,
    /// Breakdown of memory counts by memory type.
    pub by_memory_type: Vec<MemoryTypeCount>,
    /// Number of superseded memories.
    pub superseded_count: u64,
}

/// A tag and its associated memory count.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct TagCount {
    /// The tag name.
    pub tag: String,
    /// Number of memories with this tag.
    pub count: u64,
}

/// An agent label and its associated memory count.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AgentCount {
    /// The agent provenance label.
    pub agent_label: String,
    /// Number of memories with this label.
    pub count: u64,
}

/// A memory type and its associated memory count.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MemoryTypeCount {
    /// The memory type classification.
    pub memory_type: MemoryType,
    /// Number of memories of this type.
    pub count: u64,
}

/// Response from v2 `admin_reembed`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ReembedResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Number of memories queued for re-embedding.
    pub queued: usize,
}

// -- Bulk operation types --

/// Response from v2 `admin_bulk_delete`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BulkDeleteResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Number of memories that were deleted.
    pub deleted: u64,
    /// Number of memories that matched the filter (before access checks).
    pub matched: u64,
    /// Whether the matched set was capped by the server limit. When `true`,
    /// more matching memories may remain — call again to continue.
    pub capped: bool,
}

/// Response from v2 `admin_bulk_update`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BulkUpdateResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Number of memories successfully updated.
    pub updated: u64,
    /// Number of memories where write access was denied.
    pub denied: u64,
    /// Number of memories that matched the filter (before access checks).
    pub matched: u64,
    /// Whether the matched set was capped by the server limit. When `true`,
    /// more matching memories may remain — call again to continue.
    pub capped: bool,
}

/// Whether a memory has been through the `redacted()` transform (non-owner
/// access to a `Redacted`-policy memory). Owner reads retain full metadata.
///
/// Scoring fields (`updated_at`, `confidence`, `superseded_by`) are
/// preserved internally for composite ranking but must be masked here.
const fn is_redacted_view(m: &Memory) -> bool {
    m.was_redacted
}

impl From<Memory> for MemoryBase {
    fn from(m: Memory) -> Self {
        let hide = is_redacted_view(&m);
        Self {
            id: m.id,
            content: m.content,
            tags: m.tags,
            created_at: m.created_at.to_rfc3339(),
            updated_at: if hide { m.created_at.to_rfc3339() } else { m.updated_at.to_rfc3339() },
            memory_type: m.memory_type,
            importance: m.importance.value(),
            confidence: if hide { crate::types::Confidence::DEFAULT.value() } else { m.confidence.value() },
            has_embedding: m.has_embedding,
            impression_count: m.impression_count,
            superseded_by: if hide { None } else { m.superseded_by },
            entities: m.entities,
        }
    }
}

// -- Wave 4: Consolidation + Audit params --

const fn default_similarity_threshold() -> f64 {
    0.85
}

const fn default_consolidate_limit() -> usize {
    10
}

const fn default_dry_run() -> bool {
    true
}

const fn default_history_limit() -> usize {
    50
}

/// Response from v2 `admin_consolidate`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ConsolidateResponse {
    /// Action-oriented operation summary.
    pub operation: OperationSummary,
    /// Groups of near-duplicate memories found.
    pub groups: Vec<DuplicateGroupEntry>,
    /// Whether merging was performed.
    pub merged: bool,
}

/// A group of near-duplicate memories in the consolidation response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct DuplicateGroupEntry {
    /// The most recently accessed memory in the group (kept as canonical).
    pub representative_id: MemoryId,
    /// All memory IDs in the group, including the representative.
    pub member_ids: Vec<MemoryId>,
    /// Average pairwise cosine similarity within the group.
    pub similarity: f64,
    /// Number of members in the group.
    pub member_count: usize,
}

/// Response from v2 `admin_history`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct HistoryResponse {
    /// Audit log entries for the requested memory.
    pub entries: Vec<AuditEntryResponse>,
}

/// A single audit log entry in the history response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AuditEntryResponse {
    /// What happened: `store`, `update`, `delete`, `supersede`, `consolidate`, etc.
    pub action: crate::types::AuditAction,
    /// Trusted principal that performed the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// When it happened (RFC 3339).
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Action-specific context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl From<Memory> for MemoryEntry {
    fn from(m: Memory) -> Self {
        let expires_at = m.expires_at.map(|dt| dt.to_rfc3339());
        let last_impressed_at = m.last_impressed_at.map(|dt| dt.to_rfc3339());
        Self {
            base: MemoryBase::from(m),
            expires_at,
            last_impressed_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- RR-062 (partial): SearchMode FromStr error path ----------------------

    #[test]
    fn search_mode_from_str_unknown_rejected() {
        let result = "unknown".parse::<SearchMode>();
        assert!(result.is_err(), "unknown search mode should be rejected");
    }
}
