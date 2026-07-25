//! Governed context identities, definitions, memberships, and storage drafts.
//!
//! Contexts are relevance metadata. They never grant access to a memory; memory
//! authorization continues to be evaluated from the memory's provenance and
//! access policy.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    str::FromStr,
};

use chrono::{DateTime, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ulid::Ulid;
use url::Url;

use crate::{
    error::ParseEnumError,
    types::{MemoryId, ScopeDefinition, normalize_context_key},
};

/// Owner used for compatibility contexts migrated from the former global scope
/// registry.
pub const LEGACY_SYSTEM_PRINCIPAL: &str = "@localhold/legacy-system";

/// Grant target used only by frozen compatibility contexts whose legacy
/// definitions were globally selectable.
pub const LEGACY_ALL_PRINCIPALS_GRANT: &str = "*";

/// Trusted local principal permitted to mutate operator-layer context policy.
///
/// The TUI principal is a local assertion rather than remote authentication,
/// so operators must still protect database access at the OS/database boundary.
pub const OPERATOR_PRINCIPAL: &str = "operator";

/// Compatibility value returned for memories with no governed memberships.
pub const UNRESOLVED_CONTEXT_KEY: &str = "inbox/unresolved";

/// Choose the effective legacy scope using Rust's Unicode-aware whitespace
/// rules. Metadata remains primary, with provenance as the fallback.
pub(crate) fn effective_legacy_scope_key(metadata_scope: Option<&str>, provenance_scope: Option<&str>) -> Option<String> {
    metadata_scope
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| provenance_scope.map(str::trim).filter(|value| !value.is_empty()))
        .map(ToOwned::to_owned)
}

/// Maximum number of exact locators accepted in one context envelope.
pub const MAX_CONTEXT_REFS: usize = 32;
/// Maximum number of weak hints accepted in one context envelope.
pub const MAX_CONTEXT_HINTS: usize = 32;
/// Maximum length of a context key, alias, hint, or resolver query.
pub const MAX_CONTEXT_SURFACE_LEN: usize = 512;
/// Maximum length of a context display name.
pub const MAX_CONTEXT_DISPLAY_NAME_LEN: usize = 256;
/// Maximum length of an optional context description or guidance string.
pub const MAX_CONTEXT_DESCRIPTION_LEN: usize = 4_096;
/// Maximum length accepted for a raw identity value before normalization.
pub const MAX_CONTEXT_IDENTITY_VALUE_LEN: usize = 4_096;
/// Maximum number of duplicate candidates that creation may require callers to confirm.
pub const MAX_CONTEXT_CONFIRMATIONS: usize = 5;

/// Validate the bounded legacy scope adapter before it creates governed
/// contexts, aliases, hints, or relations.
///
/// # Errors
///
/// Returns a secret-free validation message for blank or oversized fields.
pub fn validate_legacy_scope_definition(scope: &ScopeDefinition) -> Result<(), String> {
    validate_legacy_scope_key(&scope.scope_key)?;
    if scope.display_name.trim().is_empty() || scope.display_name.len() > MAX_CONTEXT_DISPLAY_NAME_LEN {
        return Err(format!("display_name must be non-empty and at most {MAX_CONTEXT_DISPLAY_NAME_LEN} bytes"));
    }
    if scope.description.as_ref().is_some_and(|value| value.len() > MAX_CONTEXT_DESCRIPTION_LEN) {
        return Err(format!("description accepts at most {MAX_CONTEXT_DESCRIPTION_LEN} bytes"));
    }
    if scope.aliases.len() > MAX_CONTEXT_REFS || scope.related.len() > MAX_CONTEXT_REFS || scope.matchers.len() > MAX_CONTEXT_HINTS {
        return Err(format!(
            "aliases and related accept at most {MAX_CONTEXT_REFS} entries; matchers accepts at most {MAX_CONTEXT_HINTS}"
        ));
    }
    if scope
        .aliases
        .iter()
        .chain(&scope.matchers)
        .any(|value| value.trim().is_empty() || value.len() > MAX_CONTEXT_SURFACE_LEN)
    {
        return Err(format!("scope aliases and matchers must be non-empty and at most {MAX_CONTEXT_SURFACE_LEN} bytes"));
    }
    for related in &scope.related {
        validate_implicit_legacy_context_key(related)?;
    }
    if let Some(parent) = &scope.parent {
        validate_implicit_legacy_context_key(parent)?;
    }
    Ok(())
}

/// Validate a legacy scope key used only as an exact compatibility selector.
///
/// # Errors
///
/// Returns a secret-free validation message for blank or oversized keys.
pub fn validate_legacy_scope_key(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_CONTEXT_SURFACE_LEN {
        return Err(format!("scope key must be non-empty and at most {MAX_CONTEXT_SURFACE_LEN} bytes"));
    }
    Ok(())
}

/// Validate a legacy key that may implicitly create a compatibility context.
///
/// # Errors
///
/// Returns a secret-free validation message when either the key or its
/// derived display-name segment exceeds the governed context limits.
pub fn validate_implicit_legacy_context_key(value: &str) -> Result<(), String> {
    validate_legacy_scope_key(value)?;
    let display_name = legacy_scope_display_name(value);
    if display_name.len() > MAX_CONTEXT_DISPLAY_NAME_LEN {
        return Err(format!("implicit compatibility context display name must be at most {MAX_CONTEXT_DISPLAY_NAME_LEN} bytes"));
    }
    Ok(())
}

/// Derive the bounded human-readable name used for a raw legacy scope.
///
/// The complete legacy key remains the immutable compatibility key; only its
/// final non-empty path segment becomes the display name.
#[must_use]
pub fn legacy_scope_display_name(value: &str) -> String {
    value.trim().rsplit('/').find(|part| !part.is_empty()).unwrap_or_else(|| value.trim()).to_owned()
}

/// Stable opaque identifier for a governed context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct ContextId(Ulid);

impl ContextId {
    /// Generate a new context identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for ContextId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for ContextId {
    type Err = ulid::DecodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_str(value)?))
    }
}

impl JsonSchema for ContextId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ContextId")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A ULID-formatted opaque context identifier"
        })
    }
}

/// Stable context kind. Built-in kinds and operator-defined kinds use the same
/// representation so adding a kind does not require a wire-protocol change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct ContextKind(String);

impl ContextKind {
    /// Built-in custom context kind.
    pub const CUSTOM: &'static str = "custom";
    /// Built-in domain context kind.
    pub const DOMAIN: &'static str = "domain";
    /// Built-in organization context kind.
    pub const ORGANIZATION: &'static str = "organization";
    /// Built-in project context kind.
    pub const PROJECT: &'static str = "project";

    /// Validate and normalize a context kind.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is blank, too long, or contains
    /// characters outside the stable kind-key alphabet.
    pub fn new<S: Into<String>>(value: S) -> Result<Self, ParseEnumError> {
        let value = value.into().trim().to_ascii_lowercase();
        let valid = !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'));
        if !valid {
            return Err(ParseEnumError("context kind must contain 1-64 lowercase ASCII letters, digits, '-' or '_'".into()));
        }
        Ok(Self(value))
    }

    /// Return the normalized kind string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Infer the compatibility kind for a legacy scope key.
    #[must_use]
    pub fn from_legacy_scope(key: &str) -> Self {
        let prefix = normalize_context_key(key).split_once('/').map_or(String::new(), |(prefix, _rest)| prefix.to_owned());
        match prefix.as_str() {
            Self::PROJECT => Self(Self::PROJECT.into()),
            Self::DOMAIN => Self(Self::DOMAIN.into()),
            Self::ORGANIZATION | "org" => Self(Self::ORGANIZATION.into()),
            _ => Self(Self::CUSTOM.into()),
        }
    }

    /// Return the built-in custom kind for internal compatibility adapters.
    #[must_use]
    pub(crate) fn custom() -> Self {
        Self(Self::CUSTOM.into())
    }
}

impl fmt::Display for ContextKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ContextKind {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ContextKind {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<String> for ContextKind {
    type Error = ParseEnumError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ContextKind> for String {
    fn from(kind: ContextKind) -> Self {
        kind.0
    }
}

/// Lifecycle state for a context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextLifecycle {
    /// Selectable for new writes and reads.
    #[default]
    Active,
    /// Retained for history and identity reservation but not selectable until
    /// explicitly reactivated.
    Archived,
}

impl fmt::Display for ContextLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => f.write_str("active"),
            Self::Archived => f.write_str("archived"),
        }
    }
}

impl FromStr for ContextLifecycle {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            other => Err(ParseEnumError(format!("unknown context lifecycle: {other:?}"))),
        }
    }
}

/// One durable governed context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextDefinition {
    /// Stable opaque ID.
    pub id: ContextId,
    /// Stable kind.
    pub kind: ContextKind,
    /// Immutable human-readable key.
    pub key: String,
    /// Display name.
    pub display_name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Principal that owns and manages this context.
    pub owner_principal: String,
    /// Optional guidance returned during resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    /// Optional parent context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<ContextId>,
    /// Lifecycle state.
    pub lifecycle: ContextLifecycle,
    /// Frozen contexts are legacy compatibility definitions and cannot be
    /// modified through ordinary context tools.
    pub frozen: bool,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last definition change.
    pub updated_at: DateTime<Utc>,
}

/// Safe identity metadata persisted for a context.
///
/// Raw identity values are normalized and fingerprinted before this type
/// reaches the store. Only a redacted label is retained for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextIdentity {
    /// Identity scheme (`git_remote`, `uri`, or `namespaced_id`).
    pub scheme: String,
    /// Required namespace for `namespaced_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Stable cryptographic fingerprint of the normalized identity.
    pub fingerprint: String,
    /// Secret-free display label.
    pub redacted_label: String,
}

/// Typed durable identity supplied by an agent.
///
/// The raw value exists only while the request is normalized. Persistence and
/// responses use [`ContextIdentity`], which contains a fingerprint and a
/// redacted label instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextIdentityInput {
    /// Identity scheme: `git_remote`, `uri`, or `namespaced_id`.
    pub scheme: String,
    /// Raw identity value. This is never persisted.
    pub value: String,
    /// Required only for `namespaced_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// One context locator in a shared context envelope.
///
/// Exactly one locator form is valid: `id`, `kind` + `key`, or `kind` +
/// `identity`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextReference {
    /// Stable opaque context ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ContextId>,
    /// Context kind for key or identity locators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ContextKind>,
    /// Immutable human-readable context key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Typed durable identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<ContextIdentityInput>,
}

impl ContextReference {
    /// Validate the exactly-one-locator contract.
    ///
    /// # Errors
    ///
    /// Returns an error unless exactly one supported locator form is present.
    pub fn validate(&self) -> Result<(), String> {
        match (&self.id, &self.kind, &self.key, &self.identity) {
            (Some(_), None, None, None) => Ok(()),
            (None, Some(_), None, Some(identity)) => validate_identity_input_limits(identity),
            (None, Some(_), Some(key), None) if !key.trim().is_empty() && key.len() <= MAX_CONTEXT_SURFACE_LEN => Ok(()),
            _ => Err("a context ref must contain exactly one locator: id, kind+key, or kind+identity".into()),
        }
    }
}

/// Normalized exact locator used by persistence backends for indexed context
/// resolution. Key lookups include aliases; identity lookups use only the
/// fingerprinted form.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContextExactLookup {
    /// Stable opaque context ID.
    Id(ContextId),
    /// Normalized key or alias, optionally constrained to one kind.
    Key {
        /// Optional context-kind constraint.
        kind: Option<ContextKind>,
        /// Canonical normalized lookup value.
        normalized_key: String,
    },
    /// Fingerprinted typed identity constrained to one kind.
    Identity {
        /// Required context kind.
        kind: ContextKind,
        /// Normalized, fingerprinted identity.
        identity: ContextIdentity,
    },
}

/// Context-selection envelope shared by context-aware tools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextEnvelope {
    /// Exact context references.
    #[serde(default)]
    pub refs: Vec<ContextReference>,
    /// Weak resolver hints. Local paths belong here, never in identities.
    #[serde(default)]
    pub hints: Vec<String>,
    /// Include descendants of explicitly selected contexts.
    #[serde(default)]
    pub include_descendants: bool,
    /// Permit a governed write to remain contextless.
    #[serde(default)]
    pub allow_unresolved: bool,
}

impl ContextEnvelope {
    /// Validate bounded context request surfaces before resolution work begins.
    ///
    /// # Errors
    ///
    /// Returns a secret-free message when counts or string lengths exceed the
    /// protocol limits.
    pub fn validate_limits(&self) -> Result<(), String> {
        if self.refs.len() > MAX_CONTEXT_REFS {
            return Err(format!("context.refs accepts at most {MAX_CONTEXT_REFS} entries"));
        }
        if self.hints.len() > MAX_CONTEXT_HINTS {
            return Err(format!("context.hints accepts at most {MAX_CONTEXT_HINTS} entries"));
        }
        for reference in &self.refs {
            reference.validate()?;
        }
        if self.hints.iter().any(|hint| hint.len() > MAX_CONTEXT_SURFACE_LEN) {
            return Err(format!("each context hint accepts at most {MAX_CONTEXT_SURFACE_LEN} bytes"));
        }
        Ok(())
    }
}

/// Safe context descriptor returned on memory cards and resolution results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextDescriptor {
    /// Stable opaque ID.
    pub id: ContextId,
    /// Context kind.
    pub kind: ContextKind,
    /// Immutable context key.
    pub key: String,
    /// Human-readable display name.
    pub display_name: String,
}

impl From<&ContextDefinition> for ContextDescriptor {
    fn from(context: &ContextDefinition) -> Self {
        Self {
            id: context.id,
            kind: context.kind.clone(),
            key: context.key.clone(),
            display_name: context.display_name.clone(),
        }
    }
}

/// A context definition plus safe resolver metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextRecord {
    /// Context definition.
    pub context: ContextDefinition,
    /// Human-readable aliases.
    pub aliases: Vec<String>,
    /// Fingerprinted durable identities.
    pub identities: Vec<ContextIdentity>,
    /// Weak resolver hints.
    pub hints: Vec<String>,
}

/// One context kind managed through the operator TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextKindDefinition {
    /// Stable normalized kind.
    pub kind: ContextKind,
    /// Human-readable name.
    pub display_name: String,
    /// Built-in kinds cannot be removed.
    pub builtin: bool,
    /// Disabled kinds remain stored but cannot be selected or created.
    pub enabled: bool,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last definition change.
    pub updated_at: DateTime<Utc>,
}

/// TUI-authored context kind mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextKindDraft {
    /// Stable normalized kind.
    pub kind: ContextKind,
    /// Human-readable name.
    pub display_name: String,
    /// Whether the kind can be selected.
    pub enabled: bool,
}

/// Persisted policy layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextPolicyLayer {
    /// Operator hard ceilings and defaults.
    Operator,
    /// Per-principal customization within operator ceilings.
    Principal,
}

impl fmt::Display for ContextPolicyLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operator => f.write_str("operator"),
            Self::Principal => f.write_str("principal"),
        }
    }
}

impl FromStr for ContextPolicyLayer {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "operator" => Ok(Self::Operator),
            "principal" => Ok(Self::Principal),
            other => Err(ParseEnumError(format!("unknown context policy layer: {other:?}"))),
        }
    }
}

/// Policy controls for one context kind.
///
/// Every field is optional so the operator, principal, and anchor layers can
/// state only their differences from inherited policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ContextKindPolicy {
    /// Whether contexts of this kind may be selected.
    pub allowed: Option<bool>,
    /// Whether governed writes require a context of this kind.
    pub required: Option<bool>,
    /// Kinds that may accompany this kind on one memory. Omission is
    /// unrestricted; an empty list forbids companions.
    pub allowed_companion_kinds: Option<Vec<ContextKind>>,
    /// Durable identity schemes accepted for this kind.
    pub allowed_identity_schemes: Option<Vec<String>>,
    /// Whether ordinary agents may create this kind.
    pub agent_creation: Option<bool>,
    /// Whether agent-created contexts of this kind need a durable identity.
    pub require_identity: Option<bool>,
    /// Safe default context selected when the caller supplies no locator.
    pub default_context_id: Option<ContextId>,
    /// Default descendant expansion for this kind.
    pub include_descendants: Option<bool>,
    /// Guidance returned to agents.
    pub guidance: Option<String>,
}

impl ContextKindPolicy {
    /// Validate normalized set fields and scalar text.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported or duplicate identity schemes,
    /// duplicate companion kinds, or blank guidance.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(schemes) = &self.allowed_identity_schemes {
            validate_identity_schemes(schemes)?;
        }
        if let Some(kinds) = &self.allowed_companion_kinds {
            let mut unique = HashSet::new();
            if kinds.iter().any(|kind| !unique.insert(kind.clone())) {
                return Err("allowed_companion_kinds must be unique".into());
            }
        }
        if self.guidance.as_deref().is_some_and(|guidance| guidance.trim().is_empty()) {
            return Err("policy guidance cannot be blank".into());
        }
        Ok(())
    }
}

fn validate_identity_schemes(schemes: &[String]) -> Result<(), String> {
    let mut unique = HashSet::new();
    for scheme in schemes {
        let normalized = scheme.trim().to_ascii_lowercase();
        if !matches!(normalized.as_str(), "git_remote" | "uri" | "namespaced_id") {
            return Err("allowed_identity_schemes contains an unsupported scheme".into());
        }
        if !unique.insert(normalized) {
            return Err("allowed_identity_schemes must be unique".into());
        }
    }
    Ok(())
}

/// Stored operator or principal policy for one kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextKindPolicyRecord {
    /// Policy layer.
    pub layer: ContextPolicyLayer,
    /// Empty for the operator layer; otherwise the customized principal.
    pub principal: String,
    /// Governed kind.
    pub kind: ContextKind,
    /// Layer-local policy.
    pub policy: ContextKindPolicy,
    /// Last update.
    pub updated_at: DateTime<Utc>,
}

/// TUI-authored operator or principal policy mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextKindPolicyDraft {
    /// Policy layer.
    pub layer: ContextPolicyLayer,
    /// Empty for operator policy; otherwise the customized principal.
    pub principal: String,
    /// Governed kind.
    pub kind: ContextKind,
    /// Replacement policy.
    pub policy: ContextKindPolicy,
}

impl ContextKindPolicyDraft {
    /// Build a replacement policy draft.
    #[must_use]
    pub fn new<S: Into<String>>(layer: ContextPolicyLayer, principal: S, kind: ContextKind, policy: ContextKindPolicy) -> Self {
        Self {
            layer,
            principal: principal.into(),
            kind,
            policy,
        }
    }
}

/// Per-kind overrides attached to one active anchor context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ContextAnchorPolicy {
    /// Overrides keyed by normalized context kind.
    pub kinds: BTreeMap<String, ContextKindPolicy>,
}

impl ContextAnchorPolicy {
    /// Validate kind keys and nested policy documents.
    ///
    /// # Errors
    ///
    /// Returns an error for non-normalized kind keys or invalid nested policy.
    pub fn validate(&self) -> Result<(), String> {
        for (kind, policy) in &self.kinds {
            let normalized = ContextKind::new(kind.clone()).map_err(|error| error.to_string())?;
            if normalized.as_str() != kind {
                return Err("anchor policy kind keys must already be normalized".into());
            }
            policy.validate()?;
        }
        Ok(())
    }
}

/// Stored anchor override visible to its principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextAnchorPolicyRecord {
    /// Context that activates this override.
    pub anchor_context_id: ContextId,
    /// Principal receiving the override.
    pub principal: String,
    /// Per-kind overrides.
    pub policy: ContextAnchorPolicy,
    /// Last update.
    pub updated_at: DateTime<Utc>,
}

/// TUI-authored active-anchor policy mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextAnchorPolicyDraft {
    /// Context that activates the override.
    pub anchor_context_id: ContextId,
    /// Principal receiving the override.
    pub principal: String,
    /// Replacement policy.
    pub policy: ContextAnchorPolicy,
}

/// TUI-authored mutable context-definition fields.
///
/// The context ID, owner, kind, and human-readable key remain immutable.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextDefinitionPatch {
    /// Replacement display name.
    pub display_name: String,
    /// Replacement optional description.
    pub description: Option<String>,
    /// Replacement optional agent guidance.
    pub guidance: Option<String>,
    /// Complete alias set.
    pub aliases: Vec<String>,
    /// Complete fingerprinted identity set.
    pub identities: Vec<ContextIdentity>,
    /// Complete weak resolver-hint set.
    pub resolver_hints: Vec<String>,
}

/// Fully merged policy used for one kind and active anchor set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[expect(clippy::struct_excessive_bools, reason = "effective policy exposes four independent governed capabilities")]
pub struct EffectiveContextPolicy {
    /// Governed kind.
    pub kind: ContextKind,
    /// Whether the kind can be selected.
    pub allowed: bool,
    /// Whether a governed write must include this kind.
    pub required: bool,
    /// Allowed companion kinds, or `None` when unrestricted.
    pub allowed_companion_kinds: Option<Vec<ContextKind>>,
    /// Accepted durable identity schemes.
    pub allowed_identity_schemes: Vec<String>,
    /// Whether ordinary agents may create this kind.
    pub agent_creation: bool,
    /// Whether creation needs a durable identity.
    pub require_identity: bool,
    /// Safe default, when configured.
    pub default_context_id: Option<ContextId>,
    /// Descendant expansion default.
    pub include_descendants: bool,
    /// Layered guidance.
    pub guidance: Vec<String>,
    /// Conflicting equally-specific anchor defaults.
    pub ambiguities: Vec<String>,
}

/// Merge operator, principal, and equally-specific active-anchor policies.
///
/// Operator denials form ceilings. Principal values customize defaults within
/// those ceilings. Anchor scalar values replace inherited defaults; set values
/// intersect and denials win.
#[must_use]
#[expect(clippy::too_many_lines, reason = "policy precedence is kept in one linear merge so deny/default ordering remains reviewable")]
pub fn evaluate_context_policy(
    kind: &ContextKind,
    operator: Option<&ContextKindPolicy>,
    principal: Option<&ContextKindPolicy>,
    anchors: &[&ContextKindPolicy],
) -> EffectiveContextPolicy {
    let default_project = kind.as_str() == ContextKind::PROJECT;
    let operator_allowed = operator.and_then(|policy| policy.allowed).unwrap_or(true);
    let operator_creation_ceiling = operator.and_then(|policy| policy.agent_creation) != Some(false);
    let operator_identity_required = operator.and_then(|policy| policy.require_identity) == Some(true);

    let mut allowed = operator_allowed;
    let mut required = operator.and_then(|policy| policy.required).unwrap_or(false);
    let mut agent_creation = operator.and_then(|policy| policy.agent_creation).unwrap_or(default_project);
    let mut require_identity = operator.and_then(|policy| policy.require_identity).unwrap_or(default_project);
    let mut default_context_id = operator.and_then(|policy| policy.default_context_id);
    let mut include_descendants = operator.and_then(|policy| policy.include_descendants).unwrap_or(false);
    let mut guidance = operator
        .and_then(|policy| policy.guidance.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    let mut allowed_companion_kinds = operator
        .and_then(|policy| policy.allowed_companion_kinds.as_deref())
        .map(|values| values.iter().cloned().collect::<BTreeSet<_>>());
    let mut allowed_identity_schemes = operator
        .and_then(|policy| policy.allowed_identity_schemes.as_deref())
        .map_or_else(default_identity_schemes, normalized_scheme_set);

    if let Some(policy) = principal {
        if policy.allowed == Some(false) {
            allowed = false;
        }
        if operator_allowed {
            allowed = policy.allowed.unwrap_or(allowed);
        }
        if operator.and_then(|policy| policy.required) != Some(true) {
            required = policy.required.unwrap_or(required);
        }
        if operator_creation_ceiling {
            agent_creation = policy.agent_creation.unwrap_or(agent_creation);
        } else {
            agent_creation = false;
        }
        if !operator_identity_required {
            require_identity = policy.require_identity.unwrap_or(require_identity);
        }
        default_context_id = policy.default_context_id.or(default_context_id);
        include_descendants = policy.include_descendants.unwrap_or(include_descendants);
        if let Some(value) = policy.guidance.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            guidance.push(value.to_owned());
        }
        intersect_optional_kind_set(&mut allowed_companion_kinds, policy.allowed_companion_kinds.as_deref());
        if let Some(schemes) = policy.allowed_identity_schemes.as_deref() {
            allowed_identity_schemes = allowed_identity_schemes.intersection(&normalized_scheme_set(schemes)).cloned().collect();
        }
    }

    let mut ambiguities = Vec::new();
    if !anchors.is_empty() {
        merge_anchor_bool("required", anchors.iter().filter_map(|policy| policy.required), &mut required, &mut ambiguities);
        merge_anchor_bool(
            "require_identity",
            anchors.iter().filter_map(|policy| policy.require_identity),
            &mut require_identity,
            &mut ambiguities,
        );
        merge_anchor_bool(
            "include_descendants",
            anchors.iter().filter_map(|policy| policy.include_descendants),
            &mut include_descendants,
            &mut ambiguities,
        );
        if anchors.iter().any(|policy| policy.allowed == Some(false)) {
            allowed = false;
        } else if operator_allowed && anchors.iter().any(|policy| policy.allowed == Some(true)) {
            allowed = true;
        }
        if anchors.iter().any(|policy| policy.agent_creation == Some(false)) || !operator_creation_ceiling {
            agent_creation = false;
        } else if anchors.iter().any(|policy| policy.agent_creation == Some(true)) {
            agent_creation = true;
        }
        let anchor_defaults = anchors.iter().filter_map(|policy| policy.default_context_id).collect::<BTreeSet<_>>();
        match anchor_defaults.len() {
            0 => {}
            1 => default_context_id = anchor_defaults.first().copied(),
            _ => ambiguities.push("active anchor policies specify conflicting default_context_id values".into()),
        }
        for policy in anchors {
            intersect_optional_kind_set(&mut allowed_companion_kinds, policy.allowed_companion_kinds.as_deref());
            if let Some(schemes) = policy.allowed_identity_schemes.as_deref() {
                allowed_identity_schemes = allowed_identity_schemes.intersection(&normalized_scheme_set(schemes)).cloned().collect();
            }
            if let Some(value) = policy.guidance.as_deref().map(str::trim).filter(|value| !value.is_empty())
                && !guidance.iter().any(|existing| existing == value)
            {
                guidance.push(value.to_owned());
            }
        }
    }

    if !operator_allowed {
        allowed = false;
    }
    if operator.and_then(|policy| policy.required) == Some(true) {
        required = true;
    }
    if operator_identity_required {
        require_identity = true;
    }
    if !operator_creation_ceiling {
        agent_creation = false;
    }

    EffectiveContextPolicy {
        kind: kind.clone(),
        allowed,
        required,
        allowed_companion_kinds: allowed_companion_kinds.map(|values| values.into_iter().collect()),
        allowed_identity_schemes: allowed_identity_schemes.into_iter().collect(),
        agent_creation,
        require_identity,
        default_context_id,
        include_descendants,
        guidance,
        ambiguities,
    }
}

fn default_identity_schemes() -> BTreeSet<String> {
    ["git_remote", "namespaced_id", "uri"].into_iter().map(ToOwned::to_owned).collect()
}

fn normalized_scheme_set(values: &[String]) -> BTreeSet<String> {
    values.iter().map(|value| value.trim().to_ascii_lowercase()).collect()
}

fn intersect_optional_kind_set(current: &mut Option<BTreeSet<ContextKind>>, next: Option<&[ContextKind]>) {
    let Some(next) = next else {
        return;
    };
    let next = next.iter().cloned().collect::<BTreeSet<_>>();
    *current = Some(current.take().map_or_else(|| next.clone(), |current| current.intersection(&next).cloned().collect()));
}

fn merge_anchor_bool(field: &str, values: impl Iterator<Item = bool>, target: &mut bool, ambiguities: &mut Vec<String>) {
    let values = values.collect::<BTreeSet<_>>();
    match values.len() {
        0 => {}
        1 => *target = values.first().copied().unwrap_or(*target),
        _ => ambiguities.push(format!("active anchor policies specify conflicting {field} values")),
    }
}

/// Normalize and fingerprint a typed context identity.
///
/// # Errors
///
/// Returns a secret-free validation error when the scheme or shape is not
/// eligible for durable identity storage.
pub fn normalize_context_identity(input: &ContextIdentityInput) -> Result<ContextIdentity, String> {
    validate_identity_input_limits(input)?;
    let scheme = input.scheme.trim().to_ascii_lowercase();
    let namespace = input.namespace.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let (canonical, label_prefix, stored_namespace) = match scheme.as_str() {
        "git_remote" => {
            if namespace.is_some() {
                return Err("git_remote does not accept namespace".into());
            }
            let (canonical, label) = normalize_git_remote(&input.value)?;
            (canonical, label, None)
        }
        "uri" => {
            if namespace.is_some() {
                return Err("uri does not accept namespace".into());
            }
            let (canonical, label) = normalize_uri(&input.value)?;
            (canonical, label, None)
        }
        "namespaced_id" => {
            let namespace = namespace.ok_or_else(|| "namespaced_id requires namespace".to_owned())?;
            if !valid_namespace(namespace) || input.value.trim().is_empty() {
                return Err("namespaced_id requires a valid namespace and non-empty opaque value".into());
            }
            let canonical = format!("{namespace}\u{0}{}", input.value.trim());
            (canonical, format!("{namespace}:…"), Some(namespace.to_owned()))
        }
        _ => return Err("unsupported context identity scheme".into()),
    };
    let fingerprint = hex_lower(&Sha256::digest(format!("{scheme}\u{0}{canonical}").as_bytes()));
    let redacted_label = format!("{label_prefix}{}", fingerprint.get(..8).unwrap_or(&fingerprint));
    Ok(ContextIdentity {
        scheme,
        namespace: stored_namespace,
        fingerprint,
        redacted_label,
    })
}

fn validate_identity_input_limits(input: &ContextIdentityInput) -> Result<(), String> {
    if input.scheme.len() > 32 {
        return Err("context identity scheme is too long".into());
    }
    if input.value.len() > MAX_CONTEXT_IDENTITY_VALUE_LEN {
        return Err(format!("context identity value accepts at most {MAX_CONTEXT_IDENTITY_VALUE_LEN} bytes"));
    }
    if input.namespace.as_ref().is_some_and(|namespace| namespace.len() > 128) {
        return Err("context identity namespace accepts at most 128 bytes".into());
    }
    Ok(())
}

fn valid_namespace(value: &str) -> bool {
    value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':'))
}

fn normalize_git_remote(raw: &str) -> Result<(String, String), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("git_remote value cannot be blank".into());
    }
    let parsed = if raw.contains("://") {
        Url::parse(raw).map_err(|_error| "git_remote must be an absolute remote URL".to_owned())?
    } else {
        let (authority, path) = raw
            .split_once(':')
            .filter(|(authority, path)| authority.contains('@') && !path.is_empty())
            .ok_or_else(|| "git_remote must be an absolute URL or SSH scp-style remote".to_owned())?;
        Url::parse(&format!("ssh://{authority}/{path}")).map_err(|_error| "git_remote SSH form is invalid".to_owned())?
    };
    if parsed.scheme() == "file" {
        return Err("local paths are resolver hints, not durable identities".into());
    }
    let host = parsed.host_str().ok_or_else(|| "git_remote requires a host".to_owned())?.to_ascii_lowercase();
    let mut path = parsed.path().trim_matches('/').to_owned();
    let suffix_start = path.len().saturating_sub(4);
    if path.get(suffix_start..).is_some_and(|suffix| suffix.eq_ignore_ascii_case(".git")) {
        path.truncate(suffix_start);
    }
    if path.is_empty() {
        return Err("git_remote requires a repository path".into());
    }
    let authority = parsed.port().map_or_else(|| host.clone(), |port| format!("{host}:{port}"));
    let canonical = format!("{authority}/{path}");
    Ok((canonical, format!("{authority}/…#")))
}

fn normalize_uri(raw: &str) -> Result<(String, String), String> {
    let mut parsed = Url::parse(raw.trim()).map_err(|_error| "uri must be absolute".to_owned())?;
    if !matches!(parsed.scheme(), "https" | "ssh" | "git") {
        return Err("uri scheme is not allowed by the default identity policy".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("uri credentials, query, and fragment are not permitted".into());
    }
    if parsed.host_str().is_none() {
        return Err("uri requires an authority host".into());
    }
    let _removed = parsed.set_username("");
    let _removed = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    let canonical = parsed.to_string();
    let host = parsed.host_str().unwrap_or("host");
    Ok((canonical, format!("{}://{host}/…#", parsed.scheme())))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4_i32)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

/// Explicit permission to select a context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextGrant {
    /// Governed context.
    pub context_id: ContextId,
    /// Principal allowed to use the context.
    pub grantee_principal: String,
    /// Principal that created the grant.
    pub granted_by: String,
    /// Grant creation time.
    pub created_at: DateTime<Utc>,
}

/// Ordered direct membership between a memory and a context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct MemoryContext {
    /// Memory receiving the membership.
    pub memory_id: MemoryId,
    /// Direct context.
    pub context: ContextDefinition,
    /// Zero-based direct-membership order. Ordinal zero is the compatibility
    /// primary context.
    pub ordinal: u32,
}

/// Fully normalized input for transactional context creation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextCreateDraft {
    /// Caller-generated stable ID.
    pub id: ContextId,
    /// Validated context kind.
    pub kind: ContextKind,
    /// Immutable display key.
    pub key: String,
    /// Normalized key used for exact matching.
    pub normalized_key: String,
    /// Human display name.
    pub display_name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Owner principal.
    pub owner_principal: String,
    /// Optional resolution guidance.
    pub guidance: Option<String>,
    /// Optional parent.
    pub parent_id: Option<ContextId>,
    /// Initial aliases paired with their normalized forms.
    pub aliases: Vec<(String, String)>,
    /// Fingerprinted durable identities.
    pub identities: Vec<ContextIdentity>,
    /// Weak resolver hints. These are not identities.
    pub resolver_hints: Vec<String>,
    /// Current fuzzy candidate IDs explicitly confirmed as distinct.
    pub confirm_distinct_from: Vec<ContextId>,
    /// Require transactional fuzzy-candidate revalidation for this
    /// agent-originated creation.
    pub enforce_fuzzy_confirmation: bool,
    /// Whether the definition is a frozen compatibility context.
    pub frozen: bool,
}

impl ContextCreateDraft {
    /// Build the minimal private, mutable context draft. Callers may then add
    /// aliases, normalized identities, hints, hierarchy, and confirmations.
    #[must_use]
    pub fn private<K: Into<String>, D: Into<String>, P: Into<String>>(id: ContextId, kind: ContextKind, key: K, display_name: D, owner_principal: P) -> Self {
        let key = key.into();
        Self {
            id,
            kind,
            normalized_key: normalize_context_key(&key),
            key,
            display_name: display_name.into(),
            description: None,
            owner_principal: owner_principal.into(),
            guidance: None,
            parent_id: None,
            aliases: Vec::new(),
            identities: Vec::new(),
            resolver_hints: Vec::new(),
            confirm_distinct_from: Vec::new(),
            enforce_fuzzy_confirmation: false,
            frozen: false,
        }
    }
}

/// Compute the duplicate-protection similarity between a proposed context and
/// one existing key/display-name/alias set.
#[must_use]
pub fn context_duplicate_similarity(query: &str, key: &str, display_name: &str, aliases: &[String]) -> f64 {
    ContextSimilarityQuery::new(query).score(key, display_name, aliases)
}

/// Precomputed duplicate-query features reused while scoring a context
/// catalog.
#[derive(Debug, Clone)]
pub struct ContextSimilarityQuery {
    tokens: HashSet<String>,
    trigrams: HashSet<String>,
}

impl ContextSimilarityQuery {
    /// Prepare one bounded query for repeated candidate comparisons.
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            tokens: context_tokens(query),
            trigrams: context_trigrams(query),
        }
    }

    /// Score one context's key, display name, and aliases.
    #[must_use]
    pub fn score(&self, key: &str, display_name: &str, aliases: &[String]) -> f64 {
        std::iter::once(key)
            .chain(std::iter::once(display_name))
            .chain(aliases.iter().map(String::as_str))
            .map(|surface| normalized_set_overlap(&self.tokens, &context_tokens(surface)).max(normalized_set_overlap(&self.trigrams, &context_trigrams(surface))))
            .fold(0.0_f64, f64::max)
    }
}

fn context_tokens(value: &str) -> HashSet<String> {
    normalize_context_key(value)
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn context_trigrams(value: &str) -> HashSet<String> {
    let normalized = normalize_context_key(value);
    let chars = normalized.chars().collect::<Vec<_>>();
    if chars.len() < 3 {
        return (!normalized.is_empty()).then_some(normalized).into_iter().collect();
    }
    chars.windows(3).map(|window| window.iter().collect()).collect()
}

#[expect(clippy::float_arithmetic, reason = "duplicate protection uses bounded overlap ratios")]
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "context token/trigram sets are bounded by validated context surface lengths"
)]
fn normalized_set_overlap(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    let denominator = left.len().max(right.len());
    if denominator == 0 {
        return 0.0;
    }
    left.intersection(right).count() as f64 / denominator as f64
}

/// Context audit event written in the same transaction as its mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContextAuditDraft {
    /// Principal that performed the action.
    pub actor_principal: String,
    /// Stable action code.
    pub action: String,
    /// Optional affected context.
    pub context_id: Option<ContextId>,
    /// Optional affected memory.
    pub memory_id: Option<MemoryId>,
    /// Secret-free structured details.
    pub details: Option<serde_json::Value>,
}

impl ContextAuditDraft {
    /// Build a secret-free audit draft without an affected object.
    #[must_use]
    pub fn new<P: Into<String>, A: Into<String>>(actor_principal: P, action: A) -> Self {
        Self {
            actor_principal: actor_principal.into(),
            action: action.into(),
            context_id: None,
            memory_id: None,
            details: None,
        }
    }

    /// Attach the affected context.
    #[must_use]
    pub const fn with_context(mut self, context_id: ContextId) -> Self {
        self.context_id = Some(context_id);
        self
    }
}

/// Stored context audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ContextAuditEvent {
    /// Monotonic backend-local event ID.
    pub id: i64,
    /// Principal that performed the action.
    pub actor_principal: String,
    /// Stable action code.
    pub action: String,
    /// Optional affected context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    /// Optional affected memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<MemoryId>,
    /// Event time.
    pub timestamp: DateTime<Utc>,
    /// Secret-free structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_kind_accepts_builtins_and_user_defined_values() {
        for value in ["project", "domain", "organization", "custom", "release_train"] {
            let kind = ContextKind::new(value).unwrap();
            assert_eq!(kind.as_str(), value);
        }
        let _blank_error = ContextKind::new("").unwrap_err();
        let _spaces_error = ContextKind::new("has spaces").unwrap_err();
        let normalized = ContextKind::new("MixedCase").unwrap();
        assert_eq!(normalized.as_str(), "mixedcase");
    }

    #[test]
    fn legacy_kind_inference_is_compatibility_only() {
        assert_eq!(ContextKind::from_legacy_scope("project/localhold").as_str(), ContextKind::PROJECT);
        assert_eq!(ContextKind::from_legacy_scope("org/gearbox").as_str(), ContextKind::ORGANIZATION);
        assert_eq!(ContextKind::from_legacy_scope("arbitrary/value").as_str(), ContextKind::CUSTOM);
    }

    #[test]
    fn implicit_legacy_contexts_bound_the_derived_display_name() {
        let oversized_segment = "x".repeat(MAX_CONTEXT_DISPLAY_NAME_LEN + 1);
        let key = format!("legacy/{oversized_segment}");

        let error = validate_implicit_legacy_context_key(&key).unwrap_err();

        assert!(error.contains("display name"));
        assert!(error.contains(&MAX_CONTEXT_DISPLAY_NAME_LEN.to_string()));
    }

    #[test]
    fn git_remote_identity_removes_credentials_transport_suffix_and_query() {
        let https = normalize_context_identity(&ContextIdentityInput {
            scheme: "git_remote".into(),
            value: "https://token@example.COM/Org/Repo.git?credential=secret#fragment".into(),
            namespace: None,
        })
        .unwrap();
        let ssh = normalize_context_identity(&ContextIdentityInput {
            scheme: "git_remote".into(),
            value: "git@example.com:Org/Repo".into(),
            namespace: None,
        })
        .unwrap();

        assert_eq!(https.fingerprint, ssh.fingerprint);
        assert!(https.redacted_label.starts_with("example.com/\u{2026}#"));
        assert!(!https.redacted_label.contains("Repo"));
        assert!(!format!("{https:?}").contains("token"));
        assert!(!format!("{https:?}").contains("secret"));
    }

    #[test]
    fn git_remote_identity_normalizes_suffix_case_without_folding_repository_path() {
        let upper_suffix = normalize_context_identity(&ContextIdentityInput {
            scheme: "git_remote".into(),
            value: "https://example.com/Acme/Widget.GIT".into(),
            namespace: None,
        })
        .unwrap();
        let no_suffix = normalize_context_identity(&ContextIdentityInput {
            scheme: "git_remote".into(),
            value: "git@example.com:Acme/Widget".into(),
            namespace: None,
        })
        .unwrap();
        let different_path_case = normalize_context_identity(&ContextIdentityInput {
            scheme: "git_remote".into(),
            value: "https://example.com/acme/widget.git".into(),
            namespace: None,
        })
        .unwrap();

        assert_eq!(upper_suffix, no_suffix);
        assert_ne!(upper_suffix, different_path_case);
    }

    #[test]
    fn uri_identity_rejects_credential_query_fragment_and_local_paths() {
        for value in [
            "https://user@example.com/resource",
            "https://example.com/resource?secret=1",
            "https://example.com/resource#private",
            "/home/person/project",
            "file:///home/person/project",
        ] {
            let error = normalize_context_identity(&ContextIdentityInput {
                scheme: "uri".into(),
                value: value.into(),
                namespace: None,
            })
            .unwrap_err();
            assert!(!error.contains(value));
        }
        let identity = normalize_context_identity(&ContextIdentityInput {
            scheme: "uri".into(),
            value: "https://example.com/share/SECRET_TOKEN".into(),
            namespace: None,
        })
        .unwrap();
        assert!(identity.redacted_label.starts_with("https://example.com/\u{2026}#"));
        assert!(!identity.redacted_label.contains("SECRET_TOKEN"));
    }

    #[test]
    fn namespaced_identity_never_exposes_opaque_value() {
        let identity = normalize_context_identity(&ContextIdentityInput {
            scheme: "namespaced_id".into(),
            value: "customer-secret-42".into(),
            namespace: Some("tracker".into()),
        })
        .unwrap();

        assert_eq!(identity.namespace.as_deref(), Some("tracker"));
        assert!(identity.redacted_label.starts_with("tracker:\u{2026}"));
        assert!(!format!("{identity:?}").contains("customer-secret-42"));
    }

    #[test]
    fn policy_layers_honor_operator_ceilings_and_intersect_sets() {
        let kind = ContextKind::new("domain").unwrap();
        let operator = ContextKindPolicy {
            agent_creation: Some(false),
            allowed_identity_schemes: Some(vec!["uri".into(), "namespaced_id".into()]),
            allowed_companion_kinds: Some(vec![ContextKind::new("project").unwrap(), ContextKind::new("organization").unwrap()]),
            guidance: Some("operator guidance".into()),
            ..ContextKindPolicy::default()
        };
        let principal = ContextKindPolicy {
            agent_creation: Some(true),
            allowed_identity_schemes: Some(vec!["git_remote".into(), "uri".into()]),
            allowed_companion_kinds: Some(vec![ContextKind::new("project").unwrap()]),
            guidance: Some("principal guidance".into()),
            ..ContextKindPolicy::default()
        };

        let effective = evaluate_context_policy(&kind, Some(&operator), Some(&principal), &[]);

        assert!(!effective.agent_creation);
        assert_eq!(effective.allowed_identity_schemes, vec!["uri"]);
        assert_eq!(effective.allowed_companion_kinds, Some(vec![ContextKind::new("project").unwrap()]));
        assert_eq!(effective.guidance, vec!["operator guidance", "principal guidance"]);
    }

    #[test]
    fn anchor_cannot_relax_operator_required_context_kind() {
        let kind = ContextKind::new("domain").unwrap();
        let operator = ContextKindPolicy {
            required: Some(true),
            ..ContextKindPolicy::default()
        };
        let anchor = ContextKindPolicy {
            required: Some(false),
            ..ContextKindPolicy::default()
        };

        let effective = evaluate_context_policy(&kind, Some(&operator), None, &[&anchor]);

        assert!(effective.required);
    }

    #[test]
    fn equally_specific_anchor_denials_win_and_scalar_conflicts_are_reported() {
        let kind = ContextKind::new("project").unwrap();
        let first = ContextKindPolicy {
            allowed: Some(true),
            include_descendants: Some(true),
            allowed_identity_schemes: Some(vec!["git_remote".into(), "uri".into()]),
            ..ContextKindPolicy::default()
        };
        let second = ContextKindPolicy {
            allowed: Some(false),
            include_descendants: Some(false),
            allowed_identity_schemes: Some(vec!["uri".into()]),
            ..ContextKindPolicy::default()
        };

        let effective = evaluate_context_policy(&kind, None, None, &[&first, &second]);

        assert!(!effective.allowed);
        assert_eq!(effective.allowed_identity_schemes, vec!["uri"]);
        assert!(effective.ambiguities.iter().any(|ambiguity| ambiguity.contains("include_descendants")));
    }

    #[test]
    fn context_reference_requires_exactly_one_locator() {
        ContextReference {
            id: Some(ContextId::new()),
            ..ContextReference::default()
        }
        .validate()
        .unwrap();
        ContextReference {
            kind: Some(ContextKind::new("project").unwrap()),
            key: Some("project/localhold".into()),
            ..ContextReference::default()
        }
        .validate()
        .unwrap();
        assert!(ContextReference::default().validate().is_err());
        assert!(
            ContextReference {
                id: Some(ContextId::new()),
                key: Some("project/localhold".into()),
                ..ContextReference::default()
            }
            .validate()
            .is_err()
        );
    }
}
