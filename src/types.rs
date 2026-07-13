//! Core domain types — memory identity, access policies, filters, and search results.

use std::{borrow::Borrow, fmt, str::FromStr};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::ParseEnumError;

// ---------------------------------------------------------------------------
// Memory type classification
// ---------------------------------------------------------------------------

/// Classifies the kind of knowledge a memory captures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryType {
    /// Facts, definitions, or general knowledge.
    #[default]
    Semantic,
    /// Events that happened at a specific time.
    Episodic,
    /// Step-by-step instructions or how-tos.
    Procedural,
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Semantic => f.write_str("semantic"),
            Self::Episodic => f.write_str("episodic"),
            Self::Procedural => f.write_str("procedural"),
        }
    }
}

impl FromStr for MemoryType {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "semantic" => Ok(Self::Semantic),
            "episodic" => Ok(Self::Episodic),
            "procedural" => Ok(Self::Procedural),
            other => Err(ParseEnumError(format!("unknown memory type: {other:?}"))),
        }
    }
}

/// A validated, trimmed agent identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl AgentId {
    /// Create a new agent ID, trimming whitespace. Returns `None` if blank after trimming.
    #[must_use]
    pub fn new<S: Into<String>>(s: S) -> Option<Self> {
        let trimmed = s.into().trim().to_owned();
        if trimmed.is_empty() { None } else { Some(Self(trimmed)) }
    }

    /// Returns the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

impl TryFrom<String> for AgentId {
    type Error = ParseEnumError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s).ok_or_else(|| ParseEnumError("agent ID cannot be blank".into()))
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for AgentId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A validated, trimmed conversation/scope key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ConversationKey(String);

impl ConversationKey {
    /// Create a new conversation key, trimming whitespace. Returns `None` if blank after trimming.
    #[must_use]
    pub fn new<S: Into<String>>(s: S) -> Option<Self> {
        let trimmed = s.into().trim().to_owned();
        if trimmed.is_empty() { None } else { Some(Self(trimmed)) }
    }

    /// Returns the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConversationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ConversationKey> for String {
    fn from(key: ConversationKey) -> Self {
        key.0
    }
}

impl TryFrom<String> for ConversationKey {
    type Error = ParseEnumError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s).ok_or_else(|| ParseEnumError("conversation key cannot be blank".into()))
    }
}

impl AsRef<str> for ConversationKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ConversationKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Search mode — both the caller's requested intent and the engine's actual execution path.
///
/// `Auto` means "let the engine decide" (hybrid semantic + keyword with RRF fusion).
/// The remaining variants (`Semantic`, `Keyword`, `Text`, `Hybrid`) describe concrete
/// execution paths. When used as a request, `Auto` is the default; when used in
/// responses, the variant reflects how the search was actually executed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SearchMode {
    /// Let the engine decide (hybrid semantic + keyword, falling back as needed).
    /// In responses, this variant is never used — it resolves to one of the concrete modes.
    #[default]
    Auto,
    /// Embedding-based approximate nearest neighbor search.
    Semantic,
    /// Substring-based text matching fallback.
    Text,
    /// FTS5 full-text keyword search with BM25 ranking.
    Keyword,
    /// Hybrid search: semantic + keyword results fused with Reciprocal Rank Fusion.
    Hybrid,
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Semantic => f.write_str("semantic"),
            Self::Text => f.write_str("text"),
            Self::Keyword => f.write_str("keyword"),
            Self::Hybrid => f.write_str("hybrid"),
        }
    }
}

impl FromStr for SearchMode {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "auto" | "" => Ok(Self::Auto),
            "semantic" => Ok(Self::Semantic),
            "text" => Ok(Self::Text),
            "keyword" => Ok(Self::Keyword),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(ParseEnumError(format!("unknown search mode: {other:?}"))),
        }
    }
}

/// Unique memory identifier backed by a ULID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct MemoryId(Ulid);

impl JsonSchema for MemoryId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("MemoryId")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A ULID-formatted unique memory identifier"
        })
    }
}

impl MemoryId {
    /// Generate a new unique memory ID.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }

    /// Returns the underlying ULID value.
    #[must_use]
    pub const fn ulid(self) -> Ulid {
        self.0
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for MemoryId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_str(s)?))
    }
}

impl From<Ulid> for MemoryId {
    fn from(ulid: Ulid) -> Self {
        Self(ulid)
    }
}

// ---------------------------------------------------------------------------
// Entity tagging
// ---------------------------------------------------------------------------

/// A validated, trimmed entity type identifier.
///
/// Parallel to [`AgentId`]: trims whitespace, rejects blank values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EntityType(String);

impl EntityType {
    /// Create a new entity type, trimming whitespace. Returns `None` if blank after trimming.
    #[must_use]
    pub fn new<S: Into<String>>(s: S) -> Option<Self> {
        let trimmed = s.into().trim().to_owned();
        if trimmed.is_empty() { None } else { Some(Self(trimmed)) }
    }

    /// Returns the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<EntityType> for String {
    fn from(et: EntityType) -> Self {
        et.0
    }
}

impl TryFrom<String> for EntityType {
    type Error = ParseEnumError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s).ok_or_else(|| ParseEnumError("entity type cannot be blank".into()))
    }
}

impl AsRef<str> for EntityType {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for EntityType {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl JsonSchema for EntityType {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("EntityType")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A validated entity type identifier"
        })
    }
}

/// A caller-provided typed entity attached to a memory for entity-based retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Entity {
    /// The entity name (e.g., "Alice", "example-agent", "RFC 9110").
    pub name: String,
    /// The entity type (e.g., "person", "project", "document").
    #[serde(rename = "type")]
    pub entity_type: EntityType,
}

impl fmt::Display for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.entity_type)
    }
}

impl Entity {
    /// Create a new entity, trimming whitespace from both name and type.
    /// Returns `None` if either field is blank after trimming or contains
    /// ASCII control characters (bytes `0x00`-`0x1F`).
    #[must_use]
    pub fn new<N: Into<String>, T: Into<String>>(name: N, entity_type: T) -> Option<Self> {
        let name = name.into();
        let entity_type = entity_type.into();
        let (name, entity_type) = crate::validation::normalize_entity_parts(&name, &entity_type).ok()?;
        Some(Self { name, entity_type })
    }
}

/// An importance score clamped to the range `[0.0, 1.0]`.
///
/// Non-finite values (`NaN`, `Infinity`) are replaced with the default of 0.5.
/// Serializes transparently as an `f64` for wire compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Importance(f64);

impl Importance {
    /// Default importance value for new memories.
    pub const DEFAULT: Self = Self(0.5);

    /// Create a new `Importance`, clamping to `[0.0, 1.0]`.
    ///
    /// Non-finite values are replaced with the default (0.5).
    /// Delegates to [`crate::validation::clamp_importance`] which is the
    /// single source of truth for the clamping logic.
    #[must_use]
    pub const fn new(value: f64) -> Self {
        Self(crate::validation::clamp_importance(value))
    }

    /// Returns the inner `f64` value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }
}

impl Default for Importance {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl PartialEq for Importance {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl fmt::Display for Importance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<f64> for Importance {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

impl From<Importance> for f64 {
    fn from(imp: Importance) -> Self {
        imp.0
    }
}

/// Source trustworthiness in the range `[0.0, 1.0]`.
///
/// - 0.9+: direct quotes, verified facts
/// - 0.8: default for new memories
/// - 0.5: plausible summaries, inferred context
/// - 0.25: speculative or uncertain
///
/// Non-finite values (`NaN`, `Infinity`) are replaced with the default of 0.8.
/// Serializes transparently as an `f64` for wire compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f64);

impl Confidence {
    /// Default confidence value for new memories.
    pub const DEFAULT: Self = Self(0.8);

    /// Create a new `Confidence`, clamping to `[0.0, 1.0]`.
    ///
    /// Non-finite values are replaced with the default (0.8).
    #[must_use]
    pub const fn new(value: f64) -> Self {
        if !value.is_finite() {
            return Self::DEFAULT;
        }
        // Manual clamp for const fn (f64::clamp is not const).
        let v = if value < 0.0_f64 {
            0.0_f64
        } else if value > 1.0_f64 {
            1.0_f64
        } else {
            value
        };
        Self(v)
    }

    /// Returns the inner `f64` value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }
}

impl Default for Confidence {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<f64> for Confidence {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

impl From<Confidence> for f64 {
    fn from(c: Confidence) -> Self {
        c.0
    }
}

/// A single memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Memory {
    /// Unique identifier for this memory.
    pub id: MemoryId,
    /// The textual content of this memory.
    pub content: String,
    /// User-defined tags for categorization and filtering.
    pub tags: Vec<String>,
    /// Who or what created this memory.
    pub provenance: Provenance,
    /// Visibility policy controlling who can read this memory.
    pub access_policy: AccessPolicy,
    /// Timestamp when this memory was created.
    pub created_at: DateTime<Utc>,
    /// Timestamp when this memory's content was last modified.
    /// Defaults to `created_at` for new memories; updated automatically
    /// when `content` changes via `revise` or internal update paths.
    pub updated_at: DateTime<Utc>,
    /// When this memory expires (if ever).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether this memory has an embedding stored.
    #[serde(default)]
    pub has_embedding: bool,
    /// Classification of the memory content.
    #[serde(default)]
    pub memory_type: MemoryType,
    /// Importance score in the range `[0.0, 1.0]`. Higher values indicate
    /// greater importance and boost composite search scores.
    #[serde(default)]
    pub importance: Importance,
    /// Source trustworthiness in the range `[0.0, 1.0]`.
    #[serde(default)]
    pub confidence: Confidence,
    /// Number of times this memory has been shown in a search result.
    #[serde(default)]
    pub impression_count: u64,
    /// Timestamp of the last time this memory was shown in a search result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_impressed_at: Option<DateTime<Utc>>,
    /// ID of the memory that supersedes this one (soft versioning).
    /// When set, this memory has been replaced by a newer version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MemoryId>,
    /// Decayed activity mass from real use events (not search impressions).
    /// Updated via `record_memory_use`; drives the activity ranking signal.
    #[serde(default)]
    pub activity_mass: f64,
    /// Timestamp of the last real use event (explicit read, citation, or confirmation).
    /// Distinct from `last_impressed_at` which tracks search impressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    /// Typed entities attached to this memory for entity-based retrieval.
    /// Stored in a junction table, not in the memories row itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
    /// Whether this memory has been through `redacted()`. Used by the
    /// serialization boundary to distinguish owner reads (full access)
    /// from non-owner reads (redacted view) on `Redacted`-policy memories.
    #[serde(skip)]
    pub was_redacted: bool,
}

impl Memory {
    /// Construct a memory with all fields for testing and benchmarking.
    ///
    /// Only available with the `testing` feature or in `#[cfg(test)]` mode.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn new_for_test(content: String, tags: Vec<String>, provenance: Provenance, access_policy: AccessPolicy) -> Self {
        let now = Utc::now();
        Self {
            id: MemoryId::new(),
            content,
            tags,
            provenance,
            access_policy,
            created_at: now,
            updated_at: now,
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::DEFAULT,
            confidence: Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    /// Evaluate this memory's access policy against a caller.
    ///
    /// A blank caller (`Some("")` or whitespace-only) is treated as unauthenticated (`None`).
    #[must_use]
    pub fn check_access_level(&self, caller: Option<&str>) -> AccessLevel {
        // Normalize blank callers to None so `Some("")` cannot bypass restrictions.
        let caller = caller.filter(|s| !s.trim().is_empty());
        match &self.access_policy {
            AccessPolicy::Public => AccessLevel::Full,
            AccessPolicy::Restricted { allowed } => {
                let Some(caller) = caller else {
                    return AccessLevel::Denied;
                };
                if self.provenance.source_agent.as_deref() == Some(caller) || allowed.iter().any(|a| a == caller) {
                    AccessLevel::Full
                } else {
                    AccessLevel::Denied
                }
            }
            AccessPolicy::Redacted { .. } => {
                let Some(caller) = caller else {
                    return AccessLevel::Denied;
                };
                if self.provenance.source_agent.as_deref() == Some(caller) {
                    AccessLevel::Full
                } else {
                    AccessLevel::Redacted
                }
            }
        }
    }

    /// Return whether a redacted view keeps a specific field visible.
    /// Full-access memory values are always visible; denied access is handled before a view is built.
    #[must_use]
    pub fn field_visible_in_view(&self, field: &RedactableField) -> bool {
        if !self.was_redacted {
            return true;
        }
        match &self.access_policy {
            AccessPolicy::Redacted { visible_fields } => visible_fields.contains(field),
            AccessPolicy::Public | AccessPolicy::Restricted { .. } => true,
        }
    }

    /// Return whether this memory's content may participate in content-derived retrieval for a caller.
    #[must_use]
    pub fn content_searchable_by(&self, caller: Option<&str>) -> bool {
        match self.check_access_level(caller) {
            AccessLevel::Full => true,
            AccessLevel::Redacted => match &self.access_policy {
                AccessPolicy::Redacted { visible_fields } => visible_fields.contains(&RedactableField::Content),
                AccessPolicy::Public | AccessPolicy::Restricted { .. } => false,
            },
            AccessLevel::Denied => false,
        }
    }

    /// Check if a caller has write access to this memory.
    ///
    /// Reads may be broader (e.g. redacted views), but writes are intentionally
    /// narrower: public memories are owner-only when an owner exists (otherwise
    /// any authenticated caller may maintain them); redacted memories always
    /// require an explicit owner match; restricted memories allow owner +
    /// explicitly allowed agents.
    #[must_use]
    pub fn has_write_access(&self, caller: &str) -> bool {
        let owner = self.provenance.source_agent.as_deref();
        let is_owner = owner == Some(caller);
        match &self.access_policy {
            // Ownerless legacy/public memories should remain maintainable:
            // if no owner is set, allow authenticated callers to write.
            AccessPolicy::Public => owner.is_none() || is_owner,
            // Redacted memories require an explicit owner — ownerless redacted
            // memories deny all writes to prevent privilege escalation.
            AccessPolicy::Redacted { .. } => is_owner,
            AccessPolicy::Restricted { allowed } => is_owner || allowed.iter().any(|a| a == caller),
        }
    }

    /// Apply field-level redaction based on this memory's access policy.
    ///
    /// Always-visible fields: `id`, `access_policy`, `created_at`, `has_embedding`, `importance`.
    /// Redactable fields: `content`, `tags`, `provenance`, `entities`, `expires_at`.
    #[must_use]
    pub fn redacted(self) -> Self {
        // Destructure to break the borrow on `access_policy` before rebuilding.
        let Self {
            id,
            content,
            tags,
            provenance,
            access_policy,
            created_at,
            updated_at,
            has_embedding,
            memory_type,
            importance,
            confidence,
            entities,
            expires_at,
            superseded_by,
            activity_mass,
            last_used_at,
            // impression_count and last_impressed_at are analytics-only and
            // not needed for ranking; safe to zero in the redacted view.
            ..
        } = self;
        let visible: &[RedactableField] = match &access_policy {
            AccessPolicy::Redacted { visible_fields } => visible_fields,
            AccessPolicy::Public | AccessPolicy::Restricted { .. } => &[],
        };
        // Linear scan is efficient for the small number of redactable fields (<=6).
        let has_field = |field: &RedactableField| visible.contains(field);
        Self {
            content: if has_field(&RedactableField::Content) { content } else { "[redacted]".into() },
            tags: if has_field(&RedactableField::Tags) { tags } else { Vec::new() },
            provenance: if has_field(&RedactableField::Provenance) { provenance } else { Provenance::default() },
            expires_at: if has_field(&RedactableField::ExpiresAt) { expires_at } else { None },
            entities: if has_field(&RedactableField::Entities) { entities } else { Vec::new() },
            // Preserve scoring-relevant fields so composite ranking (which
            // runs after redaction in the search pipeline) uses real values
            // instead of defaults. These are operational signals, not
            // user-facing content — zeroing them causes redacted memories to
            // rank incorrectly relative to their replacements.
            importance,
            confidence,
            updated_at,
            superseded_by,
            activity_mass,
            last_used_at,
            id,
            access_policy,
            created_at,
            has_embedding,
            memory_type,
            impression_count: 0,
            last_impressed_at: None,
            was_redacted: true,
        }
    }

    /// Prepare a memory for wire serialization by zeroing scoring-internal
    /// fields on redacted memories. `Memory::redacted()` preserves these for
    /// composite ranking, but they must not appear in API responses.
    #[must_use]
    pub const fn sanitize_for_wire(mut self) -> Self {
        if self.was_redacted {
            self.updated_at = self.created_at;
            self.confidence = Confidence::DEFAULT;
            self.superseded_by = None;
            self.activity_mass = 0.0_f64;
            self.last_used_at = None;
        }
        self
    }

    /// Apply access policy: returns `Some(memory)` (possibly redacted) or `None` if denied.
    #[must_use]
    pub fn apply_access_policy(self, caller: Option<&str>) -> Option<Self> {
        match self.check_access_level(caller) {
            AccessLevel::Full => Some(self),
            AccessLevel::Redacted => Some(self.redacted()),
            AccessLevel::Denied => None,
        }
    }
}

/// Who/what created a memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Provenance {
    /// The agent that created this memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent: Option<String>,
    /// The current conversation/scope key used for filtering and sharing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_conversation: Option<String>,
    /// The conversation where this memory originated. Preserved across scope reassignments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_conversation: Option<String>,
    /// The user on whose behalf this memory was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user: Option<String>,
}

impl Provenance {
    /// Construct a provenance with all fields for testing and benchmarking.
    ///
    /// Only available with the `testing` feature or in `#[cfg(test)]` mode.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn new_for_test(source_agent: Option<String>, source_conversation: Option<String>, origin_conversation: Option<String>) -> Self {
        Self {
            source_agent,
            source_conversation,
            origin_conversation,
            source_user: None,
        }
    }
}

/// A registered scope definition used for write/read scope resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScopeDefinition {
    /// Stable scope key, e.g. `gearboxlogic/localhold`.
    pub scope_key: String,
    /// Display name for humans and agents.
    pub display_name: String,
    /// Optional scope description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Alternate names that resolve to this scope.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Path or git context substrings that resolve to this scope.
    #[serde(default)]
    pub matchers: Vec<String>,
    /// Optional parent scope key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Optional related scope keys.
    #[serde(default)]
    pub related: Vec<String>,
}

/// Non-destructive metadata attached to an existing memory row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MemoryMetadata {
    /// Memory this metadata describes.
    pub memory_id: MemoryId,
    /// Canonical scope key resolved at write or migration time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Caller-supplied compact summary. Original content remains unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Human-readable agent label for provenance. This does not grant access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Trusted server principal that created this metadata, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_principal: Option<String>,
    /// Stable quality warning codes observed at write/migration time.
    #[serde(default)]
    pub quality_flags: Vec<String>,
    /// Metadata schema version.
    pub schema_version: i64,
}

/// Partial metadata changes supplied by a revise request.
///
/// Stores merge this patch with the existing metadata row, if any, inside the
/// same transaction as the memory mutation that authorized the write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetadataPatch {
    /// Replacement canonical scope key.
    pub scope_key: Option<String>,
    /// Replacement compact summary.
    pub summary: Option<String>,
    /// Replacement human-readable agent label.
    pub agent_label: Option<String>,
}

impl MetadataPatch {
    /// Whether this patch has no changes to apply.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.scope_key.is_none() && self.summary.is_none() && self.agent_label.is_none()
    }
}

/// Byte threshold used by quality warnings and migration reporting.
pub const LARGE_CONTENT_WARNING_THRESHOLD_BYTES: usize = 4_000;

/// Conservative report for the metadata migration surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MetadataMigrationReport {
    /// Total memories in the store.
    pub total_memories: u64,
    /// Memories that already have a metadata row.
    pub metadata_rows: u64,
    /// Memories missing a metadata row.
    pub missing_metadata: u64,
    /// Memories without a caller-supplied summary.
    pub missing_summary: u64,
    /// Memories whose scope is missing or explicitly unresolved.
    pub unresolved_scope: u64,
    /// Duplicate candidates based on identical original content.
    pub duplicate_candidates: u64,
    /// Memories larger than the large-content warning threshold.
    pub oversized: u64,
    /// Memories that look like code dumps or code-derived content.
    pub code_derived: u64,
}

/// Outcome from a non-destructive metadata migration pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MetadataMigrationOutcome {
    /// Existing memories missing a metadata row before this pass.
    pub candidate_count: u64,
    /// Existing memories that already had metadata and were skipped.
    pub skipped_existing: u64,
    /// Metadata rows inserted by this pass. Dry runs always report zero.
    pub migrated: u64,
    /// Candidate memories assigned to the unresolved inbox scope.
    pub unresolved_scope: u64,
    /// Candidate memories still missing caller-supplied summaries.
    pub missing_summary: u64,
    /// Candidate memories larger than the large-content warning threshold.
    pub oversized: u64,
    /// Candidate memories that look like code dumps or code-derived content.
    pub code_derived: u64,
}

/// A field that can be redacted in the `Redacted` access policy.
///
/// Serializes/deserializes as a lowercase string for wire compatibility.
/// Unknown field names from older or future database schemas are preserved
/// via the `Unknown` variant rather than causing deserialization failures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RedactableField {
    /// The memory content text.
    Content,
    /// The tag list.
    Tags,
    /// The provenance metadata.
    Provenance,
    /// The attached entities.
    Entities,
    /// The importance score.
    ///
    /// Parsed and round-tripped for wire compatibility. Redacted reads still
    /// preserve the stored importance to avoid changing ranking behavior for
    /// existing memories whose policies predate this field.
    Importance,
    /// The expiry timestamp.
    ExpiresAt,
    /// An unrecognized field name from a newer or older schema version.
    /// Preserved for round-trip fidelity but has no effect on redaction logic.
    Unknown(String),
}

impl fmt::Display for RedactableField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Content => f.write_str("content"),
            Self::Tags => f.write_str("tags"),
            Self::Provenance => f.write_str("provenance"),
            Self::Entities => f.write_str("entities"),
            Self::Importance => f.write_str("importance"),
            Self::ExpiresAt => f.write_str("expires_at"),
            Self::Unknown(s) => f.write_str(s),
        }
    }
}

impl FromStr for RedactableField {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "content" => Self::Content,
            "tags" => Self::Tags,
            "provenance" => Self::Provenance,
            "entities" => Self::Entities,
            "importance" => Self::Importance,
            "expires_at" => Self::ExpiresAt,
            other => Self::Unknown(other.to_owned()),
        })
    }
}

impl Serialize for RedactableField {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RedactableField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        // Infallible parse: unknown fields become `Unknown(s)` for backward compatibility.
        Ok(s.parse().unwrap_or_else(|e: std::convert::Infallible| match e {}))
    }
}

impl JsonSchema for RedactableField {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("RedactableField")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A redactable field name (e.g. content, tags, provenance, entities, importance)"
        })
    }
}

/// Controls who can see a memory.
#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccessPolicy {
    /// Visible to all callers.
    #[default]
    Public,
    /// Visible only to the listed agents.
    Restricted {
        /// Agent identifiers permitted to access this memory.
        allowed: Vec<String>,
    },
    /// Visible but with some fields hidden from unauthorized callers.
    Redacted {
        /// Field names that remain visible to unauthorized callers.
        visible_fields: Vec<RedactableField>,
    },
}

impl fmt::Display for AccessPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Restricted { .. } => f.write_str("restricted"),
            Self::Redacted { .. } => f.write_str("redacted"),
        }
    }
}

/// Result of evaluating a memory's access policy against a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccessLevel {
    /// Caller has full access to all fields.
    Full,
    /// Caller can see the memory but some fields are redacted.
    Redacted,
    /// Caller cannot see this memory at all.
    Denied,
}

impl fmt::Display for AccessLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("full"),
            Self::Redacted => f.write_str("redacted"),
            Self::Denied => f.write_str("denied"),
        }
    }
}

/// Minimal authorization envelope retained after a memory row is deleted.
///
/// Tombstones intentionally exclude content, tags, entities, embeddings, and
/// metadata. They exist only so deleted-memory history can be authorized without
/// reconstructing the deleted memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryTombstone {
    /// The deleted memory ID.
    pub memory_id: MemoryId,
    /// Provenance needed to identify the owner and scope authorization context.
    pub provenance: Provenance,
    /// Access policy in effect when the memory was deleted.
    pub access_policy: AccessPolicy,
    /// When the tombstone was written.
    pub deleted_at: DateTime<Utc>,
    /// Principal that deleted the memory, when known.
    pub deleted_by_principal: Option<String>,
}

impl MemoryTombstone {
    /// Build a tombstone from the current memory authorization envelope.
    #[must_use]
    pub fn from_memory(memory: &Memory, deleted_at: DateTime<Utc>, deleted_by_principal: Option<String>) -> Self {
        Self {
            memory_id: memory.id,
            provenance: memory.provenance.clone(),
            access_policy: memory.access_policy.clone(),
            deleted_at,
            deleted_by_principal,
        }
    }

    /// Evaluate tombstone visibility using the same owner/allowlist semantics as live memories.
    #[must_use]
    pub fn check_access_level(&self, caller: Option<&str>) -> AccessLevel {
        let caller = caller.filter(|s| !s.trim().is_empty());
        match &self.access_policy {
            AccessPolicy::Public => AccessLevel::Full,
            AccessPolicy::Restricted { allowed } => {
                let Some(caller) = caller else {
                    return AccessLevel::Denied;
                };
                if self.provenance.source_agent.as_deref() == Some(caller) || allowed.iter().any(|a| a == caller) {
                    AccessLevel::Full
                } else {
                    AccessLevel::Denied
                }
            }
            AccessPolicy::Redacted { .. } => {
                let Some(caller) = caller else {
                    return AccessLevel::Denied;
                };
                if self.provenance.source_agent.as_deref() == Some(caller) {
                    AccessLevel::Full
                } else {
                    AccessLevel::Redacted
                }
            }
        }
    }
}

/// Partial update payload — only `Some` fields are applied.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MemoryUpdate {
    /// New content to replace the existing memory text.
    pub content: Option<String>,
    /// New tag set to replace existing tags.
    pub tags: Option<Vec<String>>,
    /// New access policy to replace the existing one.
    pub access_policy: Option<AccessPolicy>,
    /// New importance score in the range `[0.0, 1.0]`.
    pub importance: Option<Importance>,
    /// New confidence score in the range `[0.0, 1.0]`.
    pub confidence: Option<Confidence>,
    /// New conversation scope for filtering.
    pub source_conversation: Option<String>,
    /// New entity set to replace existing entities (DELETE + INSERT).
    pub entities: Option<Vec<Entity>>,
}

/// Contextual parameters for a query that are NOT filter criteria.
/// Separated from `MemoryFilter` to distinguish "what to find" from "who is asking".
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct QueryContext {
    /// The server-resolved principal making the request, for access policy enforcement.
    /// When `None`, only `Public` memories are visible.
    pub principal: Option<String>,
}

/// Filter criteria for listing/searching memories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryFilter {
    /// Only include memories matching all of these tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Only include memories created with this agent provenance label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// Only include memories belonging to this scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Exact-match filter applied against `provenance.origin_conversation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_scope: Option<String>,
    /// Any-match scope filter applied against `provenance.source_conversation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes_any: Option<Vec<String>>,
    /// Restrict results to a creation-time window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    /// Substring to match against memory content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_search: Option<String>,
    /// Filter to memories with or without embeddings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_embedding: Option<bool>,
    /// Maximum number of results to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Only include memories of this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<MemoryType>,
    /// When `true`, include memories that have been superseded by newer versions.
    /// Default is `false` — superseded memories are hidden from search and list results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_superseded: Option<bool>,
    /// Filter to memories tagged with this entity name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Filter to memories tagged with any of these entity names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities_any: Option<Vec<String>>,
    /// Filter to memories tagged with entities of this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
}

/// Time-range bound for filtering.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TimeRange {
    /// Inclusive lower bound (only memories created at or after this time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<DateTime<Utc>>,
    /// Exclusive upper bound (only memories created before this time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
}

/// Per-component score breakdown for debugging and tuning.
///
/// Each field is the raw component value in `[0.0, 1.0]` before weighting.
/// The final composite score is `Σ(weight_i * component_i)`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct ScoreBreakdown {
    /// Query-specific relevance Q(d): blended reranker + retrieval, or retrieval-only.
    pub query_relevance: f64,
    /// Intrinsic importance I(d): cost of omission if relevant.
    pub importance: f64,
    /// Content freshness F(d): decay from `updated_at` with per-type half-life.
    pub freshness: f64,
    /// Activity A(d): decayed true-use mass with log-saturation.
    pub activity: f64,
    /// Confidence C(d): source trustworthiness.
    pub confidence: f64,
}

/// A search result wrapping a memory with an optional similarity score.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchResult {
    /// The matched memory entry.
    pub memory: Memory,
    /// Distance/similarity score (lower = more similar for L2, higher = more similar for cosine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    /// Normalized first-stage retrieval score `H(d)` in `[0.0, 1.0]`.
    /// Present for every search mode after retrieval seeding; preserved even
    /// when reranking is enabled so query relevance can be reconstructed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_score: Option<f64>,
    /// Cross-encoder reranker score `CE(d)` in `[0.0, 1.0]`.
    /// Present only when a reranker model is configured and the search
    /// pipeline invoked cross-encoder scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reranker_score: Option<f64>,
    /// Composite score combining all ranking components on a 0-100 scale.
    /// Present after composite reranking in the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite_score: Option<f64>,
    /// Per-component score breakdown for debugging and tuning.
    /// Present after composite reranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_breakdown: Option<ScoreBreakdown>,
}

impl From<Memory> for SearchResult {
    fn from(memory: Memory) -> Self {
        Self {
            memory,
            distance: None,
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        }
    }
}

/// Aggregate statistics about stored memories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryStats {
    /// Total number of stored memories.
    pub total: u64,
    /// Number of memories that have an associated embedding.
    pub with_embedding: u64,
    /// Number of memories without an embedding.
    pub without_embedding: u64,
    /// Number of memories past their expiration time.
    pub expired: u64,
    /// Memory counts grouped by tag.
    pub by_tag: Vec<(String, u64)>,
    /// Memory counts grouped by agent provenance label.
    pub by_agent_label: Vec<(String, u64)>,
    /// Estimated database storage size in bytes (`page_count * page_size`).
    pub storage_bytes: Option<u64>,
    /// Creation timestamp of the oldest memory.
    pub oldest_memory: Option<DateTime<Utc>>,
    /// Creation timestamp of the newest memory.
    pub newest_memory: Option<DateTime<Utc>>,
    /// Number of distinct scope keys (`provenance.source_conversation`).
    pub scope_count: u64,
    /// Memory counts grouped by `memory_type`.
    pub by_memory_type: Vec<(MemoryType, u64)>,
    /// Number of memories that have been superseded.
    pub superseded_count: u64,
}

// ---------------------------------------------------------------------------
// Audit and write-outcome types (moved from store/mod.rs per RR-020)
// ---------------------------------------------------------------------------

/// Typed audit action for structured audit logging.
///
/// Each variant corresponds to a distinct engine operation that is recorded
/// in the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditAction {
    /// A new memory was stored.
    Store,
    /// An existing memory was updated.
    Update,
    /// A memory was deleted.
    Delete,
    /// A memory was superseded by a newer version.
    Supersede,
    /// Memories were deleted in bulk.
    BulkDelete,
    /// Memories were updated in bulk.
    BulkUpdate,
    /// Duplicate memories were consolidated.
    Consolidate,
    /// Memories were reassigned to a different scope.
    Reassign,
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store => f.write_str("store"),
            Self::Update => f.write_str("update"),
            Self::Delete => f.write_str("delete"),
            Self::Supersede => f.write_str("supersede"),
            Self::BulkDelete => f.write_str("bulk_delete"),
            Self::BulkUpdate => f.write_str("bulk_update"),
            Self::Consolidate => f.write_str("consolidate"),
            Self::Reassign => f.write_str("reassign"),
        }
    }
}

impl FromStr for AuditAction {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "store" => Ok(Self::Store),
            "update" => Ok(Self::Update),
            "delete" => Ok(Self::Delete),
            "supersede" => Ok(Self::Supersede),
            "bulk_delete" => Ok(Self::BulkDelete),
            "bulk_update" => Ok(Self::BulkUpdate),
            "consolidate" => Ok(Self::Consolidate),
            "reassign" => Ok(Self::Reassign),
            other => Err(ParseEnumError(format!("unknown audit action: {other:?}"))),
        }
    }
}

/// A single entry from the memory audit log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct AuditEntry {
    /// What happened.
    pub action: AuditAction,
    /// Who performed the action.
    pub caller_agent: Option<String>,
    /// When it happened.
    pub timestamp: DateTime<Utc>,
    /// Action-specific context (e.g., old content hash for updates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Audit entry data prepared before a mutation chooses its final memory IDs.
///
/// Stores pair this draft with the memory IDs that are actually mutated and
/// insert the resulting audit rows in the same transaction as the mutation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct AuditDraft {
    /// What happened.
    pub action: AuditAction,
    /// Who performed the action.
    pub caller_agent: Option<String>,
    /// When it happened.
    pub timestamp: DateTime<Utc>,
    /// Action-specific context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Outcome of an authorization-checked write operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteOutcome {
    /// The operation was applied successfully.
    Applied,
    /// The target memory was not found.
    NotFound,
    /// The caller does not have write access.
    Denied,
}

impl fmt::Display for WriteOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied => f.write_str("applied"),
            Self::NotFound => f.write_str("not_found"),
            Self::Denied => f.write_str("denied"),
        }
    }
}

/// Result of an authorization-aware memory update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuthorizedUpdateOutcome {
    /// Whether the operation succeeded, was not found, or was denied.
    pub outcome: WriteOutcome,
    /// Set when content changed and a re-embedding should be attempted.
    pub reembed_revision: Option<i64>,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn memory_id_display_parse_roundtrip() {
        let id = MemoryId::new();
        let s = id.to_string();
        let parsed: MemoryId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn memory_id_serde_roundtrip() {
        let id = MemoryId::new();
        let json = serde_json::to_string(&id).unwrap();
        let parsed: MemoryId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn access_policy_tagged_serde() {
        let public = AccessPolicy::Public;
        let json = serde_json::to_string(&public).unwrap();
        assert!(json.contains(r#""type":"public"#));

        let restricted = AccessPolicy::Restricted { allowed: vec!["agent-1".into()] };
        let json = serde_json::to_string(&restricted).unwrap();
        let parsed: AccessPolicy = serde_json::from_str(&json).unwrap();
        match parsed {
            AccessPolicy::Restricted { allowed } => assert_eq!(allowed, vec!["agent-1"]),
            #[expect(clippy::panic, reason = "test assertion for enum variant matching")]
            AccessPolicy::Public | AccessPolicy::Redacted { .. } => panic!("expected Restricted"),
        }

        // Redacted with RedactableField roundtrip
        let redacted = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content, RedactableField::Tags],
        };
        let json = serde_json::to_string(&redacted).unwrap();
        assert!(json.contains(r#""content"#));
        assert!(json.contains(r#""tags"#));
        let parsed: AccessPolicy = serde_json::from_str(&json).unwrap();
        match parsed {
            AccessPolicy::Redacted { visible_fields } => {
                assert_eq!(visible_fields, vec![RedactableField::Content, RedactableField::Tags]);
            }
            #[expect(clippy::panic, reason = "test assertion for enum variant matching")]
            AccessPolicy::Public | AccessPolicy::Restricted { .. } => panic!("expected Redacted"),
        }
    }

    #[test]
    fn redactable_field_serde_wire_compat() {
        // Ensure RedactableField serializes as simple lowercase strings (wire compatibility)
        let json = serde_json::to_string(&RedactableField::Content).unwrap();
        assert_eq!(json, r#""content""#);
        let parsed: RedactableField = serde_json::from_str(r#""entities""#).unwrap();
        assert_eq!(parsed, RedactableField::Entities);
    }

    #[test]
    fn redactable_field_unknown_roundtrip() {
        // Unknown field names from future schema versions should round-trip gracefully.
        let parsed: RedactableField = serde_json::from_str(r#""future_field""#).unwrap();
        assert_eq!(parsed, RedactableField::Unknown("future_field".into()));
        let json = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, r#""future_field""#);
    }

    #[test]
    fn provenance_default_is_empty() {
        let p = Provenance::default();
        assert!(p.source_agent.is_none());
        assert!(p.source_conversation.is_none());
        assert!(p.origin_conversation.is_none());
        assert!(p.source_user.is_none());
    }

    #[test]
    fn memory_serde_roundtrip() {
        let mem = Memory {
            id: MemoryId::new(),
            content: "test content".into(),
            tags: vec!["tag1".into()],
            provenance: Provenance {
                source_agent: Some("agent-1".into()),
                ..Default::default()
            },
            access_policy: AccessPolicy::Public,
            created_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::DEFAULT,
            confidence: Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        };
        let json = serde_json::to_string(&mem).unwrap();
        let parsed: Memory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, mem.id);
        assert_eq!(parsed.content, mem.content);
    }

    /// Build a minimal memory with the given access policy and optional owner.
    fn mem_with(policy: AccessPolicy, owner: Option<&str>) -> Memory {
        Memory {
            id: MemoryId::new(),
            content: "secret".into(),
            tags: vec!["t".into()],
            provenance: Provenance {
                source_agent: owner.map(Into::into),
                ..Default::default()
            },
            access_policy: policy,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::DEFAULT,
            confidence: Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    // --- T1: blank caller treated as unauthenticated ---

    #[test]
    fn check_access_empty_caller_denied_for_restricted() {
        let mem = mem_with(AccessPolicy::Restricted { allowed: vec!["a".into()] }, Some("owner"));
        assert_eq!(mem.check_access_level(Some("")), AccessLevel::Denied);
    }

    #[test]
    fn check_access_whitespace_caller_denied_for_restricted() {
        let mem = mem_with(AccessPolicy::Restricted { allowed: vec!["a".into()] }, Some("owner"));
        assert_eq!(mem.check_access_level(Some("  ")), AccessLevel::Denied);
    }

    #[test]
    fn check_access_empty_caller_denied_for_redacted() {
        let mem = mem_with(
            AccessPolicy::Redacted {
                visible_fields: vec![RedactableField::Content],
            },
            Some("owner"),
        );
        assert_eq!(mem.check_access_level(Some("")), AccessLevel::Denied);
    }

    #[test]
    fn check_access_none_caller_full_for_public() {
        let mem = mem_with(AccessPolicy::Public, Some("owner"));
        assert_eq!(mem.check_access_level(None), AccessLevel::Full);
    }

    // --- T2: ownerless Redacted denies writes ---

    #[test]
    fn write_access_ownerless_redacted_denied() {
        let mem = mem_with(AccessPolicy::Redacted { visible_fields: Vec::new() }, None);
        assert!(!mem.has_write_access("any-caller"));
    }

    #[test]
    fn write_access_owned_redacted_owner_allowed() {
        let mem = mem_with(AccessPolicy::Redacted { visible_fields: Vec::new() }, Some("alice"));
        assert!(mem.has_write_access("alice"));
    }

    #[test]
    fn write_access_owned_redacted_non_owner_denied() {
        let mem = mem_with(AccessPolicy::Redacted { visible_fields: Vec::new() }, Some("alice"));
        assert!(!mem.has_write_access("bob"));
    }

    #[test]
    fn write_access_ownerless_public_allowed() {
        let mem = mem_with(AccessPolicy::Public, None);
        assert!(mem.has_write_access("any-caller"));
    }

    // --- T5: redacted() uses HashSet (functional correctness) ---

    #[test]
    fn redacted_preserves_visible_fields() {
        let mem = mem_with(
            AccessPolicy::Redacted {
                visible_fields: vec![RedactableField::Content, RedactableField::Tags],
            },
            Some("owner"),
        );
        let redacted = mem.redacted();
        assert_eq!(redacted.content, "secret");
        assert_eq!(redacted.tags, vec!["t"]);
        // provenance and expires_at should be redacted
        assert!(redacted.provenance.source_agent.is_none());
        assert!(redacted.expires_at.is_none());
    }

    #[test]
    fn redacted_hides_all_when_empty_visible() {
        let mem = mem_with(AccessPolicy::Redacted { visible_fields: Vec::new() }, Some("owner"));
        let redacted = mem.redacted();
        assert_eq!(redacted.content, "[redacted]");
        assert!(redacted.tags.is_empty());
    }

    #[test]
    fn redacted_preserves_importance_for_backward_compatibility() {
        let mut mem = mem_with(AccessPolicy::Redacted { visible_fields: Vec::new() }, Some("owner"));
        mem.importance = Importance::new(0.9);

        let redacted = mem.redacted();

        assert_eq!(redacted.importance, Importance::new(0.9));
    }

    #[test]
    fn redacted_preserves_scoring_fields_and_resets_analytics() {
        let mut mem = mem_with(AccessPolicy::Redacted { visible_fields: Vec::new() }, Some("owner"));
        let custom_updated = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
        let custom_last_used = Utc.with_ymd_and_hms(2030, 1, 3, 0, 0, 0).unwrap();
        let superseder = MemoryId::new();
        mem.updated_at = custom_updated;
        mem.confidence = Confidence::new(0.2_f64);
        mem.impression_count = 99;
        mem.last_impressed_at = Some(Utc.with_ymd_and_hms(2030, 1, 2, 0, 0, 0).unwrap());
        mem.superseded_by = Some(superseder);
        mem.activity_mass = 7.5_f64;
        mem.last_used_at = Some(custom_last_used);

        let redacted = mem.redacted();

        // Scoring-relevant fields are preserved so composite ranking works
        // correctly for redacted memories in the search pipeline.
        assert_eq!(redacted.updated_at, custom_updated, "updated_at must be preserved for freshness scoring");
        assert_eq!(redacted.confidence, Confidence::new(0.2_f64), "confidence must be preserved for composite scoring");
        assert_eq!(redacted.superseded_by, Some(superseder), "superseded_by must be preserved for superseded penalty");
        assert!(
            (redacted.activity_mass - 7.5_f64).abs() < f64::EPSILON,
            "activity_mass must be preserved for activity scoring"
        );
        assert_eq!(redacted.last_used_at, Some(custom_last_used), "last_used_at must be preserved for activity decay");

        // Analytics-only fields are zeroed (not used by ranking).
        assert_eq!(redacted.impression_count, 0);
        assert!(redacted.last_impressed_at.is_none());
    }

    // --- Memory type ---

    #[test]
    fn memory_type_serde_roundtrip() {
        for mt in [MemoryType::Semantic, MemoryType::Episodic, MemoryType::Procedural] {
            let json = serde_json::to_string(&mt).unwrap();
            let parsed: MemoryType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, mt);
        }
    }

    #[test]
    fn memory_type_display_parse() {
        for mt in [MemoryType::Semantic, MemoryType::Episodic, MemoryType::Procedural] {
            let s = mt.to_string();
            let parsed: MemoryType = s.parse().unwrap();
            assert_eq!(parsed, mt);
        }
    }

    #[test]
    fn memory_type_default_is_semantic() {
        assert_eq!(MemoryType::default(), MemoryType::Semantic);
    }

    // --- Entity::new() validation ---

    #[test]
    fn entity_new_valid() {
        let e = Entity::new("Alice", "person").unwrap();
        assert_eq!(e.name, "Alice");
        assert_eq!(e.entity_type.as_str(), "person");
    }

    #[test]
    fn entity_new_trims_whitespace() {
        let e = Entity::new("  Alice  ", "  person  ").unwrap();
        assert_eq!(e.name, "Alice");
        assert_eq!(e.entity_type.as_str(), "person");
    }

    #[test]
    fn entity_new_blank_name_returns_none() {
        assert!(Entity::new("", "person").is_none());
        assert!(Entity::new("   ", "person").is_none());
    }

    #[test]
    fn entity_new_blank_type_returns_none() {
        assert!(Entity::new("Alice", "").is_none());
        assert!(Entity::new("Alice", "   ").is_none());
    }

    #[test]
    fn entity_new_rejects_control_chars_in_name() {
        assert!(Entity::new("Alice\0", "person").is_none(), "null byte in name");
        assert!(Entity::new("A\x01lice", "person").is_none(), "SOH in name");
        assert!(Entity::new("Bob\x1F", "person").is_none(), "US in name");
    }

    #[test]
    fn entity_new_rejects_control_chars_in_type() {
        assert!(Entity::new("Alice", "per\0son").is_none(), "null byte in type");
        assert!(Entity::new("Alice", "ty\x01pe").is_none(), "SOH in type");
        assert!(Entity::new("Alice", "ty\x1Fpe").is_none(), "US in type");
    }

    #[test]
    fn entity_new_accepts_printable_ascii_and_unicode() {
        let e = Entity::new("Alice!", "person-type_current").unwrap();
        assert_eq!(e.name, "Alice!");
        assert_eq!(e.entity_type.as_str(), "person-type_current");
    }

    // --- superseded_by: Option<MemoryId> serde ---

    #[test]
    fn memory_superseded_by_serde_roundtrip() {
        let mut mem = mem_with(AccessPolicy::Public, Some("owner"));
        let superseder = MemoryId::new();
        mem.superseded_by = Some(superseder);
        let json = serde_json::to_string(&mem).unwrap();
        let parsed: Memory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.superseded_by, Some(superseder));
    }

    #[test]
    fn memory_superseded_by_none_serde() {
        let mem = mem_with(AccessPolicy::Public, Some("owner"));
        let json = serde_json::to_string(&mem).unwrap();
        assert!(!json.contains("superseded_by"));
        let parsed: Memory = serde_json::from_str(&json).unwrap();
        assert!(parsed.superseded_by.is_none());
    }

    // -- RR-043: AgentId -----------------------------------------------------

    #[test]
    fn agent_id_new_empty_returns_none() {
        assert!(AgentId::new("").is_none());
    }

    #[test]
    fn agent_id_new_blank_returns_none() {
        assert!(AgentId::new("  ").is_none());
    }

    #[test]
    fn agent_id_new_trims_whitespace() {
        let id = AgentId::new(" hello ").unwrap();
        assert_eq!(id.as_str(), "hello");
    }

    #[test]
    fn agent_id_try_from_roundtrip() {
        let id = AgentId::new("test-agent").unwrap();
        let s: String = id.clone().into();
        let parsed = AgentId::try_from(s).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn agent_id_display_matches_inner() {
        let id = AgentId::new("test-agent").unwrap();
        assert_eq!(id.to_string(), "test-agent");
    }

    // -- RR-043: ConversationKey ---------------------------------------------

    #[test]
    fn conversation_key_new_empty_returns_none() {
        assert!(ConversationKey::new("").is_none());
    }

    #[test]
    fn conversation_key_new_blank_returns_none() {
        assert!(ConversationKey::new("  ").is_none());
    }

    #[test]
    fn conversation_key_new_trims_whitespace() {
        let key = ConversationKey::new(" conv-123 ").unwrap();
        assert_eq!(key.as_str(), "conv-123");
    }

    #[test]
    fn conversation_key_try_from_roundtrip() {
        let key = ConversationKey::new("scope-1").unwrap();
        let s: String = key.clone().into();
        let parsed = ConversationKey::try_from(s).unwrap();
        assert_eq!(key, parsed);
    }

    #[test]
    fn conversation_key_display_matches_inner() {
        let key = ConversationKey::new("scope-1").unwrap();
        assert_eq!(key.to_string(), "scope-1");
    }

    // -- RR-062 (partial): enum FromStr error paths for types enums ----------

    #[test]
    fn memory_type_from_str_unknown_rejected() {
        let result = "unknown".parse::<MemoryType>();
        assert!(result.is_err(), "unknown memory type should be rejected");
    }
}
