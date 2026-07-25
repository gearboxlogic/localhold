//! Governed context persistence.

use std::{
    collections::{HashMap, HashSet},
    str::FromStr as _,
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row as _, sql_str::AssertSqlSafe, types::Json};
use sqlx_postgres::{PgRow, Postgres};

use super::{ContextReader, ContextWriter, MemoryContextPresence, PostgresStore, SqliteStore, crud::fetch_memory_by_id, sqlite_write_tx};
use crate::{
    context::{
        ContextAnchorPolicy, ContextAnchorPolicyDraft, ContextAnchorPolicyRecord, ContextAuditDraft, ContextAuditEvent, ContextCreateDraft, ContextDefinition,
        ContextDefinitionPatch, ContextExactLookup, ContextGrant, ContextId, ContextIdentity, ContextKind, ContextKindDefinition, ContextKindDraft, ContextKindPolicy,
        ContextKindPolicyDraft, ContextKindPolicyRecord, ContextLifecycle, ContextPolicyLayer, ContextRecord, ContextSimilarityQuery, LEGACY_ALL_PRINCIPALS_GRANT,
        LEGACY_SYSTEM_PRINCIPAL, MAX_CONTEXT_CONFIRMATIONS, MAX_CONTEXT_DESCRIPTION_LEN, MAX_CONTEXT_DISPLAY_NAME_LEN, MAX_CONTEXT_HINTS, MAX_CONTEXT_REFS,
        MAX_CONTEXT_SURFACE_LEN, MemoryContext, OPERATOR_PRINCIPAL, UNRESOLVED_CONTEXT_KEY as UNRESOLVED_SCOPE,
    },
    error::StoreError,
    types::{AccessLevel, AccessPolicy, MemoryId, Provenance, WriteOutcome, normalize_context_key, write_access_allowed},
};

const MAX_CONTEXT_PAGE_SIZE: usize = 500;
const MAX_CONTEXT_AUDIT_PAGE_SIZE: usize = 500;

#[cfg(test)]
mod query_plan_tests {
    use rusqlite::{Connection, params};

    #[test]
    fn kind_constrained_alias_lookup_uses_alias_lookup_index() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE contexts (
                     id TEXT PRIMARY KEY,
                     kind TEXT NOT NULL,
                     normalized_key TEXT NOT NULL
                 );
                 CREATE TABLE context_aliases (
                     context_id TEXT NOT NULL,
                     normalized_alias TEXT NOT NULL
                 );
                 CREATE INDEX idx_context_aliases_lookup
                     ON context_aliases(normalized_alias, context_id);",
            )
            .unwrap();
        let details = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT alias_row.context_id
                 FROM context_aliases AS alias_row INDEXED BY idx_context_aliases_lookup
                 JOIN contexts AS candidate ON candidate.id = alias_row.context_id
                 WHERE candidate.kind = ?1
                   AND alias_row.normalized_alias = ?2",
            )
            .unwrap()
            .query_map(params!["project", "project/localhold"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(
            details.iter().any(|detail| detail.contains("idx_context_aliases_lookup")),
            "unexpected alias lookup plan: {details:?}"
        );
    }
}

const CONTEXT_COLUMNS: &str = "
    context_row.id, context_row.kind, context_row.context_key,
    context_row.display_name, context_row.description,
    context_row.owner_principal, context_row.guidance,
    context_row.parent_id, context_row.lifecycle, context_row.frozen,
    context_row.created_at, context_row.updated_at
";

#[derive(Debug)]
struct ContextRow {
    id: String,
    kind: String,
    key: String,
    display_name: String,
    description: Option<String>,
    owner_principal: String,
    guidance: Option<String>,
    parent_id: Option<String>,
    lifecycle: String,
    frozen: bool,
    created_at: String,
    updated_at: String,
}

fn read_context_row(row: &Row<'_>) -> rusqlite::Result<ContextRow> {
    Ok(ContextRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        key: row.get(2)?,
        display_name: row.get(3)?,
        description: row.get(4)?,
        owner_principal: row.get(5)?,
        guidance: row.get(6)?,
        parent_id: row.get(7)?,
        lifecycle: row.get(8)?,
        frozen: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn parse_timestamp(value: &str, field: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(format!("invalid {field} timestamp: {error}").into()))
}

fn parse_context_row(row: ContextRow) -> Result<ContextDefinition, StoreError> {
    Ok(ContextDefinition {
        id: ContextId::from_str(&row.id).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        kind: ContextKind::new(row.kind).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        key: row.key,
        display_name: row.display_name,
        description: row.description,
        owner_principal: row.owner_principal,
        guidance: row.guidance,
        parent_id: row
            .parent_id
            .map(|id| ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
            .transpose()?,
        lifecycle: ContextLifecycle::from_str(&row.lifecycle).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        frozen: row.frozen,
        created_at: parse_timestamp(&row.created_at, "contexts.created_at")?,
        updated_at: parse_timestamp(&row.updated_at, "contexts.updated_at")?,
    })
}

fn parse_sqlite_memory_context_batch_row(row: &Row<'_>, principal: &str) -> Result<Option<MemoryContext>, StoreError> {
    let provenance: Provenance = serde_json::from_str(&row.get::<_, String>(14)?)?;
    let access_policy: AccessPolicy = serde_json::from_str(&row.get::<_, String>(15)?)?;
    if !memory_read_allowed(&provenance, &access_policy, principal) {
        return Ok(None);
    }
    let memory_id = MemoryId::from_str(&row.get::<_, String>(13)?).map_err(|error| StoreError::Serialization(Box::new(error)))?;
    let ordinal: i64 = row.get(12)?;
    Ok(Some(MemoryContext {
        memory_id,
        context: parse_context_row(read_context_row(row)?)?,
        ordinal: u32::try_from(ordinal).map_err(|error| StoreError::Serialization(Box::new(error)))?,
    }))
}

fn append_memory_context(memberships: &mut HashMap<MemoryId, Vec<MemoryContext>>, membership: Option<MemoryContext>) {
    if let Some(membership) = membership {
        memberships.entry(membership.memory_id).or_default().push(membership);
    }
}

fn sqlite_usize(value: usize, field: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|error| StoreError::Conflict(format!("{field} exceeds SQLite INTEGER: {error}")))
}

fn context_visible_sql(alias: &str) -> String {
    format!(
        "({alias}.owner_principal = ?1 OR EXISTS (
            SELECT 1 FROM context_grants AS grant_row
            WHERE grant_row.context_id = {alias}.id
              AND grant_row.grantee_principal IN (?1, '{LEGACY_ALL_PRINCIPALS_GRANT}')
        ))"
    )
}

fn fetch_context_authorized(conn: &Connection, context_id: &ContextId, principal: &str) -> Result<Option<ContextDefinition>, StoreError> {
    let sql = format!(
        "SELECT {CONTEXT_COLUMNS}
         FROM contexts AS context_row
         WHERE context_row.id = ?2 AND {}",
        context_visible_sql("context_row")
    );
    let row = conn.query_row(&sql, params![principal, context_id.to_string()], read_context_row).optional()?;
    row.map(parse_context_row).transpose()
}

fn fetch_sqlite_context_record(conn: &Connection, context: ContextDefinition) -> Result<ContextRecord, StoreError> {
    let context_id = context.id.to_string();
    let aliases = {
        let mut statement = conn.prepare("SELECT alias FROM context_aliases WHERE context_id = ?1 ORDER BY normalized_alias")?;
        statement.query_map([&context_id], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?
    };
    let identities = {
        let mut statement = conn.prepare(
            "SELECT scheme, namespace, fingerprint, redacted_label
             FROM context_identities
             WHERE context_id = ?1
             ORDER BY scheme, namespace, fingerprint",
        )?;
        statement
            .query_map([&context_id], |row| {
                let namespace: String = row.get(1)?;
                Ok(ContextIdentity {
                    scheme: row.get(0)?,
                    namespace: (!namespace.is_empty()).then_some(namespace),
                    fingerprint: row.get(2)?,
                    redacted_label: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let hints = {
        let mut statement = conn.prepare("SELECT hint FROM context_resolver_hints WHERE context_id = ?1 ORDER BY normalized_hint")?;
        statement.query_map([&context_id], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(ContextRecord {
        context,
        aliases,
        identities,
        hints,
    })
}

fn fetch_sqlite_context_records(conn: &Connection, contexts: Vec<ContextDefinition>) -> Result<Vec<ContextRecord>, StoreError> {
    if contexts.is_empty() {
        return Ok(Vec::new());
    }
    let context_ids = contexts.iter().map(|context| context.id.to_string()).collect::<Vec<_>>();
    let context_ids_json = serde_json::to_string(&context_ids)?;
    let positions = context_ids.iter().enumerate().map(|(index, id)| (id.clone(), index)).collect::<HashMap<_, _>>();
    let mut records = contexts
        .into_iter()
        .map(|context| ContextRecord {
            context,
            aliases: Vec::new(),
            identities: Vec::new(),
            hints: Vec::new(),
        })
        .collect::<Vec<_>>();

    {
        let mut statement = conn.prepare(
            "SELECT context_id, alias
             FROM context_aliases
             WHERE context_id IN (SELECT value FROM json_each(?1))
             ORDER BY context_id, normalized_alias",
        )?;
        let mut rows = statement.query([&context_ids_json])?;
        while let Some(row) = rows.next()? {
            let context_id: String = row.get(0)?;
            if let Some(index) = positions.get(&context_id) {
                records[*index].aliases.push(row.get(1)?);
            }
        }
    }
    {
        let mut statement = conn.prepare(
            "SELECT context_id, scheme, namespace, fingerprint, redacted_label
             FROM context_identities
             WHERE context_id IN (SELECT value FROM json_each(?1))
             ORDER BY context_id, scheme, namespace, fingerprint",
        )?;
        let mut rows = statement.query([&context_ids_json])?;
        while let Some(row) = rows.next()? {
            let context_id: String = row.get(0)?;
            if let Some(index) = positions.get(&context_id) {
                let namespace: String = row.get(2)?;
                records[*index].identities.push(ContextIdentity {
                    scheme: row.get(1)?,
                    namespace: (!namespace.is_empty()).then_some(namespace),
                    fingerprint: row.get(3)?,
                    redacted_label: row.get(4)?,
                });
            }
        }
    }
    {
        let mut statement = conn.prepare(
            "SELECT context_id, hint
             FROM context_resolver_hints
             WHERE context_id IN (SELECT value FROM json_each(?1))
             ORDER BY context_id, normalized_hint",
        )?;
        let mut rows = statement.query([&context_ids_json])?;
        while let Some(row) = rows.next()? {
            let context_id: String = row.get(0)?;
            if let Some(index) = positions.get(&context_id) {
                records[*index].hints.push(row.get(1)?);
            }
        }
    }
    Ok(records)
}

fn context_use_allowed(tx: &Transaction<'_>, context_id: &ContextId, principal: &str, require_active: bool) -> Result<bool, StoreError> {
    let lifecycle_clause = if require_active { "AND context_row.lifecycle = 'active'" } else { "" };
    let sql = format!(
        "SELECT EXISTS(
            SELECT 1 FROM contexts AS context_row
            JOIN context_kinds AS kind_row ON kind_row.kind = context_row.kind
            WHERE context_row.id = ?2
              AND kind_row.enabled
              {lifecycle_clause}
              AND {}
         )",
        context_visible_sql("context_row")
    );
    Ok(tx.query_row(&sql, params![principal, context_id.to_string()], |row| row.get(0))?)
}

fn normalize_explicit_grantee(grantee: &str) -> Result<String, StoreError> {
    let grantee = grantee.trim();
    if grantee.is_empty() || crate::http_auth::is_reserved_principal(grantee) {
        return Err(StoreError::Conflict("ordinary context grants require one explicit non-reserved principal".into()));
    }
    Ok(grantee.to_owned())
}

fn normalize_explicit_grantees(grantees: &[String]) -> Result<Vec<String>, StoreError> {
    let mut normalized = Vec::with_capacity(grantees.len());
    let mut unique = HashSet::with_capacity(grantees.len());
    for grantee in grantees {
        let grantee = normalize_explicit_grantee(grantee)?;
        if !unique.insert(grantee.clone()) {
            return Err(StoreError::Conflict("context grant principals must be unique".into()));
        }
        normalized.push(grantee);
    }
    Ok(normalized)
}

#[expect(
    clippy::too_many_arguments,
    reason = "membership insertion validates authorization, compatibility cache, audit, and timestamp in one transaction"
)]
pub(crate) fn insert_initial_memory_contexts_sqlite(
    tx: &Transaction<'_>,
    memory_id: &MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    compatibility_scope: &str,
    audit: &ContextAuditDraft,
    now: &str,
) -> Result<(), StoreError> {
    insert_memory_contexts_sqlite(tx, memory_id, context_ids, principal, compatibility_scope, audit, now, &HashSet::new())
}

#[expect(
    clippy::too_many_arguments,
    reason = "membership insertion validates authorization, compatibility cache, preserved inactive memberships, audit, and timestamp"
)]
fn insert_memory_contexts_sqlite(
    tx: &Transaction<'_>,
    memory_id: &MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    compatibility_scope: &str,
    audit: &ContextAuditDraft,
    now: &str,
    preserved_context_ids: &HashSet<ContextId>,
) -> Result<(), StoreError> {
    validate_audit_actor(audit, principal)?;
    let unique = context_ids.iter().copied().collect::<HashSet<_>>();
    if unique.len() != context_ids.len() {
        return Err(StoreError::Conflict("memory context memberships must be unique".into()));
    }
    for context_id in context_ids {
        let require_active = !preserved_context_ids.contains(context_id);
        if !context_use_allowed(tx, context_id, principal, require_active)? {
            return Err(StoreError::Conflict(format!(
                "context {context_id} is unavailable, archived, or not granted to principal {principal:?}"
            )));
        }
    }
    let expected_scope = if let Some(primary) = context_ids.first() {
        tx.query_row("SELECT context_key FROM contexts WHERE id = ?1", [primary.to_string()], |row| row.get::<_, String>(0))?
    } else {
        UNRESOLVED_SCOPE.to_owned()
    };
    if compatibility_scope != expected_scope {
        return Err(StoreError::Conflict(format!(
            "compatibility scope {compatibility_scope:?} does not match primary governed context {expected_scope:?}"
        )));
    }
    for (ordinal, context_id) in context_ids.iter().enumerate() {
        let ordinal = sqlite_usize(ordinal, "context membership ordinal")?;
        let _inserted = tx.execute(
            "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![memory_id.to_string(), context_id.to_string(), ordinal, now],
        )?;
    }
    insert_context_audit(tx, audit, None, Some(memory_id), now)
}

#[expect(
    clippy::too_many_arguments,
    reason = "atomic membership replacement carries authorization, compatibility cache, audit, and timestamp"
)]
pub(crate) fn replace_memory_contexts_sqlite_tx(
    tx: &Transaction<'_>,
    memory_id: &MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    compatibility_scope: &str,
    audit: &ContextAuditDraft,
    now: &str,
) -> Result<(), StoreError> {
    let mut statement = tx.prepare("SELECT context_id FROM memory_contexts WHERE memory_id = ?1")?;
    let preserved_context_ids = statement
        .query_map([memory_id.to_string()], |row| row.get::<_, String>(0))?
        .map(|result| result.and_then(|id| ContextId::from_str(&id).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))))
        .collect::<Result<HashSet<_>, _>>()?;
    drop(statement);
    let _removed = tx.execute("DELETE FROM memory_contexts WHERE memory_id = ?1", [memory_id.to_string()])?;
    insert_memory_contexts_sqlite(tx, memory_id, context_ids, principal, compatibility_scope, audit, now, &preserved_context_ids)?;
    let _updated_metadata = tx.execute("UPDATE memory_metadata SET scope_key = ?1, updated_at = ?2 WHERE memory_id = ?3", params![
        compatibility_scope,
        now,
        memory_id.to_string()
    ])?;
    let _updated_memory = tx.execute(
        "UPDATE memories
         SET provenance = json_set(provenance, '$.source_conversation', ?1)
         WHERE id = ?2",
        params![compatibility_scope, memory_id.to_string()],
    )?;
    Ok(())
}

type ContextOwnerState = Option<(String, bool)>;

fn context_owner_state(tx: &Transaction<'_>, context_id: &ContextId) -> Result<ContextOwnerState, StoreError> {
    Ok(tx
        .query_row("SELECT owner_principal, frozen FROM contexts WHERE id = ?1", [context_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?)
}

fn require_mutable_owned_context(tx: &Transaction<'_>, context_id: &ContextId, principal: &str) -> Result<(), StoreError> {
    let Some((owner, frozen)) = context_owner_state(tx, context_id)? else {
        return Err(StoreError::NotFound(format!("context not found: {context_id}")));
    };
    if owner != principal {
        return Err(StoreError::Conflict(format!("principal {principal:?} does not own context {context_id}")));
    }
    if frozen {
        return Err(StoreError::Conflict(format!("context {context_id} is a frozen legacy compatibility context")));
    }
    Ok(())
}

fn validate_audit_actor(audit: &ContextAuditDraft, principal: &str) -> Result<(), StoreError> {
    if principal.trim().is_empty() || audit.actor_principal.trim().is_empty() {
        return Err(StoreError::Conflict("context audit actor and authorized principal cannot be blank".into()));
    }
    if audit.actor_principal != principal {
        return Err(StoreError::Conflict("context audit actor must match the authorized principal".into()));
    }
    Ok(())
}

fn insert_context_audit(tx: &Transaction<'_>, audit: &ContextAuditDraft, context_id: Option<&ContextId>, memory_id: Option<&MemoryId>, now: &str) -> Result<(), StoreError> {
    let details = audit.details.as_ref().map(serde_json::to_string).transpose()?;
    let _inserted = tx.execute(
        "INSERT INTO context_audit_events (
            actor_principal, action, context_id, memory_id, timestamp, details
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            audit.actor_principal,
            audit.action,
            context_id.map(ToString::to_string),
            memory_id.map(ToString::to_string),
            now,
            details,
        ],
    )?;
    Ok(())
}

fn validate_identity(identity: &ContextIdentity) -> Result<(), StoreError> {
    if !matches!(identity.scheme.as_str(), "git_remote" | "uri" | "namespaced_id") {
        return Err(StoreError::Conflict(format!("unsupported context identity scheme {:?}", identity.scheme)));
    }
    if identity.scheme == "namespaced_id" && identity.namespace.as_deref().is_none_or(str::is_empty) {
        return Err(StoreError::Conflict("namespaced_id requires a non-empty namespace".into()));
    }
    if identity.scheme != "namespaced_id" && identity.namespace.is_some() {
        return Err(StoreError::Conflict(format!("identity scheme {:?} does not accept a namespace", identity.scheme)));
    }
    if identity.fingerprint.len() != 64 || !identity.fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::Conflict(
            "context identity fingerprint must be a 64-character hexadecimal SHA-256 digest".into(),
        ));
    }
    if identity.redacted_label.trim().is_empty() || identity.redacted_label.len() > MAX_CONTEXT_DISPLAY_NAME_LEN || identity.redacted_label.chars().any(char::is_control) {
        return Err(StoreError::Conflict("context identity redacted label cannot be blank".into()));
    }
    Ok(())
}

fn validate_definition_patch_surfaces(patch: &ContextDefinitionPatch) -> Result<(), StoreError> {
    if patch.display_name.trim().is_empty() || patch.display_name.len() > MAX_CONTEXT_DISPLAY_NAME_LEN {
        return Err(StoreError::Conflict("context display name is blank or too long".into()));
    }
    if patch.description.as_ref().is_some_and(|value| value.len() > MAX_CONTEXT_DESCRIPTION_LEN)
        || patch.guidance.as_ref().is_some_and(|value| value.len() > MAX_CONTEXT_DESCRIPTION_LEN)
    {
        return Err(StoreError::Conflict("context description or guidance exceeds the supported length".into()));
    }
    if patch.aliases.len() > MAX_CONTEXT_REFS || patch.identities.len() > MAX_CONTEXT_REFS || patch.resolver_hints.len() > MAX_CONTEXT_HINTS {
        return Err(StoreError::Conflict("context aliases, identities, or hints exceed protocol limits".into()));
    }
    if patch.aliases.iter().chain(&patch.resolver_hints).any(|value| value.len() > MAX_CONTEXT_SURFACE_LEN) {
        return Err(StoreError::Conflict("context alias or hint exceeds the supported length".into()));
    }
    Ok(())
}

fn memory_read_allowed(provenance: &Provenance, access_policy: &AccessPolicy, principal: &str) -> bool {
    match access_policy {
        AccessPolicy::Public => true,
        AccessPolicy::Redacted { .. } => !principal.trim().is_empty(),
        AccessPolicy::Restricted { allowed } => {
            !principal.trim().is_empty() && (provenance.source_agent.as_deref() == Some(principal) || allowed.iter().any(|candidate| candidate == principal))
        }
    }
}

fn readable_membership_memory_id(row: (String, String, String), principal: &str) -> Result<Option<MemoryId>, StoreError> {
    let (memory_id, provenance, access_policy) = row;
    let provenance = serde_json::from_str::<Provenance>(&provenance)?;
    let access_policy = serde_json::from_str::<AccessPolicy>(&access_policy)?;
    if !memory_read_allowed(&provenance, &access_policy, principal) {
        return Ok(None);
    }
    let memory_id = MemoryId::from_str(&memory_id).map_err(|error| StoreError::Serialization(Box::new(error)))?;
    Ok(Some(memory_id))
}

fn validate_create_draft(draft: &ContextCreateDraft, audit: &ContextAuditDraft) -> Result<(), StoreError> {
    validate_audit_actor(audit, &draft.owner_principal)?;
    if draft.key.trim().is_empty() || draft.key.len() > MAX_CONTEXT_SURFACE_LEN || draft.normalized_key != normalize_context_key(&draft.key) {
        return Err(StoreError::Conflict("context key and normalized_key are inconsistent".into()));
    }
    if draft.display_name.trim().is_empty() || draft.display_name.len() > MAX_CONTEXT_DISPLAY_NAME_LEN {
        return Err(StoreError::Conflict("context display name cannot be blank".into()));
    }
    if draft.description.as_ref().is_some_and(|value| value.len() > MAX_CONTEXT_DESCRIPTION_LEN)
        || draft.guidance.as_ref().is_some_and(|value| value.len() > MAX_CONTEXT_DESCRIPTION_LEN)
    {
        return Err(StoreError::Conflict("context description or guidance exceeds the supported length".into()));
    }
    if draft.owner_principal.trim().is_empty() {
        return Err(StoreError::Conflict("context owner principal cannot be blank".into()));
    }
    if draft.frozen && draft.owner_principal != LEGACY_SYSTEM_PRINCIPAL {
        return Err(StoreError::Conflict("only the legacy system principal may create frozen compatibility contexts".into()));
    }
    if draft.parent_id == Some(draft.id) {
        return Err(StoreError::Conflict("a context cannot be its own parent".into()));
    }
    if draft.aliases.len() > MAX_CONTEXT_REFS || draft.resolver_hints.len() > MAX_CONTEXT_HINTS || draft.confirm_distinct_from.len() > MAX_CONTEXT_CONFIRMATIONS {
        return Err(StoreError::Conflict("context aliases, hints, or confirmations exceed protocol limits".into()));
    }
    let mut aliases = HashSet::new();
    for (alias, normalized) in &draft.aliases {
        if alias.trim().is_empty() || alias.len() > MAX_CONTEXT_SURFACE_LEN || *normalized != normalize_context_key(alias) || !aliases.insert(normalized) {
            return Err(StoreError::Conflict("context aliases must be non-empty, normalized, and unique".into()));
        }
    }
    if draft.resolver_hints.iter().any(|hint| hint.trim().is_empty() || hint.len() > MAX_CONTEXT_SURFACE_LEN) {
        return Err(StoreError::Conflict("context resolver hints must be non-empty and bounded".into()));
    }
    for identity in &draft.identities {
        validate_identity(identity)?;
    }
    Ok(())
}

fn validate_fuzzy_confirmation(draft: &ContextCreateDraft, existing: impl IntoIterator<Item = (ContextId, String, String, Vec<String>)>) -> Result<(), StoreError> {
    if !draft.enforce_fuzzy_confirmation {
        return Ok(());
    }
    let query = format!("{} {}", draft.key, draft.display_name);
    let similarity = ContextSimilarityQuery::new(&query);
    let mut current = existing
        .into_iter()
        .filter_map(|(id, key, display_name, aliases)| {
            let is_exact = normalize_context_key(&key) == draft.normalized_key;
            let score = similarity.score(&key, &display_name, &aliases);
            (!is_exact && score >= 0.72_f64).then_some((id, key, score))
        })
        .collect::<Vec<_>>();
    current.sort_by(|left, right| right.2.total_cmp(&left.2).then_with(|| left.1.cmp(&right.1)).then_with(|| left.0.cmp(&right.0)));
    current.dedup_by_key(|candidate| candidate.0);
    current.truncate(MAX_CONTEXT_CONFIRMATIONS);
    let mut current = current.into_iter().map(|candidate| candidate.0).collect::<Vec<_>>();
    current.sort_unstable();
    let mut confirmed = draft.confirm_distinct_from.clone();
    confirmed.sort_unstable();
    confirmed.dedup();
    if current != confirmed {
        return Err(StoreError::Conflict(
            "fuzzy context candidates changed; resolve candidates and submit a fresh confirm_distinct_from set".into(),
        ));
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "SQLite context creation keeps definition, aliases, identities, hints, and audit in one transaction"
)]
fn create_context_conn(conn: &mut Connection, draft: &ContextCreateDraft, audit: &ContextAuditDraft, now: DateTime<Utc>) -> Result<ContextDefinition, StoreError> {
    validate_create_draft(draft, audit)?;
    let tx = sqlite_write_tx(conn)?;
    let kind_enabled: bool = tx
        .query_row("SELECT enabled FROM context_kinds WHERE kind = ?1", [draft.kind.as_str()], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::Conflict(format!("unknown context kind {:?}", draft.kind.as_str())))?;
    if !kind_enabled {
        return Err(StoreError::Conflict(format!("context kind {:?} is disabled", draft.kind.as_str())));
    }
    if let Some(parent_id) = draft.parent_id
        && !context_use_allowed(&tx, &parent_id, &draft.owner_principal, true)?
    {
        return Err(StoreError::Conflict(format!("parent context {parent_id} is unavailable, archived, or not granted")));
    }
    let exact_key_or_alias: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(
                 SELECT 1
                 FROM contexts AS context_row
                 WHERE context_row.kind = ?2
                   AND {}
                   AND (
                       context_row.normalized_key = ?3 OR EXISTS (
                           SELECT 1
                           FROM context_aliases AS alias_row
                           WHERE alias_row.context_id = context_row.id
                             AND alias_row.normalized_alias = ?3
                       )
                   )
             )",
            context_visible_sql("context_row")
        ),
        params![draft.owner_principal, draft.kind.as_str(), draft.normalized_key],
        |row| row.get(0),
    )?;
    let mut exact_identity = false;
    for identity in &draft.identities {
        exact_identity = tx.query_row(
            &format!(
                "SELECT EXISTS(
                     SELECT 1
                     FROM context_identities AS identity_row
                     JOIN contexts AS context_row ON context_row.id = identity_row.context_id
                     WHERE context_row.kind = ?2
                       AND {}
                       AND identity_row.scheme = ?3
                       AND identity_row.namespace = ?4
                       AND identity_row.fingerprint = ?5
                 )",
                context_visible_sql("context_row")
            ),
            params![
                draft.owner_principal,
                draft.kind.as_str(),
                identity.scheme,
                identity.namespace.as_deref().unwrap_or_default(),
                identity.fingerprint,
            ],
            |row| row.get(0),
        )?;
        if exact_identity {
            break;
        }
    }
    if exact_key_or_alias || exact_identity {
        return Err(StoreError::Conflict("exact visible context key, alias, or identity already exists".into()));
    }
    let existing = {
        let sql = format!(
            "SELECT context_row.id, context_row.context_key, context_row.display_name,
                    COALESCE((
                        SELECT json_group_array(ordered_alias.alias)
                        FROM (
                            SELECT alias_row.alias
                            FROM context_aliases AS alias_row
                            WHERE alias_row.context_id = context_row.id
                            ORDER BY alias_row.normalized_alias
                        ) AS ordered_alias
                    ), '[]')
             FROM contexts AS context_row
             WHERE context_row.kind = ?2
               AND context_row.lifecycle = 'active'
               AND {}
             ORDER BY context_row.normalized_key, context_row.id",
            context_visible_sql("context_row")
        );
        let mut statement = tx.prepare(&sql)?;
        let rows = statement
            .query_map(params![draft.owner_principal, draft.kind.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut existing = Vec::with_capacity(rows.len());
        for (id, key, display_name, aliases) in rows {
            let aliases = serde_json::from_str(&aliases)?;
            let id = ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error)))?;
            existing.push((id, key, display_name, aliases));
        }
        existing
    };
    validate_fuzzy_confirmation(draft, existing)?;
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM contexts
            WHERE id = ?1 OR (
                owner_principal = ?2 AND kind = ?3 AND normalized_key = ?4
            )
         )",
        params![draft.id.to_string(), draft.owner_principal, draft.kind.as_str(), draft.normalized_key,],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(StoreError::Conflict("context ID or owner/kind/key already exists".into()));
    }
    let now = now.to_rfc3339();
    let _inserted = tx.execute(
        "INSERT INTO contexts (
            id, kind, context_key, normalized_key, display_name, description,
            owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10, ?11, ?11)",
        params![
            draft.id.to_string(),
            draft.kind.as_str(),
            draft.key,
            draft.normalized_key,
            draft.display_name,
            draft.description,
            draft.owner_principal,
            draft.guidance,
            draft.parent_id.map(|id| id.to_string()),
            draft.frozen,
            now,
        ],
    )?;
    for (alias, normalized) in &draft.aliases {
        let _inserted = tx.execute(
            "INSERT INTO context_aliases (context_id, alias, normalized_alias, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![draft.id.to_string(), alias, normalized, now],
        )?;
    }
    for identity in &draft.identities {
        let _inserted = tx.execute(
            "INSERT INTO context_identities (
                context_id, owner_principal, kind, scheme, namespace,
                fingerprint, redacted_label, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                draft.id.to_string(),
                draft.owner_principal,
                draft.kind.as_str(),
                identity.scheme,
                identity.namespace.as_deref().unwrap_or_default(),
                identity.fingerprint,
                identity.redacted_label,
                now,
            ],
        )?;
    }
    let mut hints = HashSet::new();
    for hint in &draft.resolver_hints {
        let normalized = normalize_context_key(hint);
        if normalized.is_empty() || !hints.insert(normalized.clone()) {
            continue;
        }
        let _inserted = tx.execute(
            "INSERT INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![draft.id.to_string(), hint, normalized, now],
        )?;
    }
    insert_context_audit(&tx, audit, Some(&draft.id), None, &now)?;
    tx.commit()?;
    Ok(ContextDefinition {
        id: draft.id,
        kind: draft.kind.clone(),
        key: draft.key.clone(),
        display_name: draft.display_name.clone(),
        description: draft.description.clone(),
        owner_principal: draft.owner_principal.clone(),
        guidance: draft.guidance.clone(),
        parent_id: draft.parent_id,
        lifecycle: ContextLifecycle::Active,
        frozen: draft.frozen,
        created_at: parse_timestamp(&now, "created context")?,
        updated_at: parse_timestamp(&now, "created context")?,
    })
}

#[expect(clippy::too_many_arguments, reason = "transactional hierarchy mutation needs target, parent, principal, audit, and timestamp")]
fn set_context_parent_conn(
    conn: &mut Connection,
    context_id: ContextId,
    parent_id: Option<ContextId>,
    principal: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    validate_audit_actor(audit, principal)?;
    let tx = sqlite_write_tx(conn)?;
    require_mutable_owned_context(&tx, &context_id, principal)?;
    if parent_id == Some(context_id) {
        return Err(StoreError::Conflict("a context cannot be its own parent".into()));
    }
    if let Some(parent_id) = parent_id {
        if !context_use_allowed(&tx, &parent_id, principal, true)? {
            return Err(StoreError::Conflict(format!("parent context {parent_id} is unavailable, archived, or not granted")));
        }
        let cycle: bool = tx.query_row(
            "WITH RECURSIVE ancestors(id, parent_id) AS (
                SELECT id, parent_id FROM contexts WHERE id = ?1
                UNION
                SELECT parent.id, parent.parent_id
                FROM contexts AS parent
                JOIN ancestors ON parent.id = ancestors.parent_id
             )
             SELECT EXISTS(SELECT 1 FROM ancestors WHERE id = ?2)",
            params![parent_id.to_string(), context_id.to_string()],
            |row| row.get(0),
        )?;
        if cycle {
            return Err(StoreError::Conflict(format!(
                "setting context {context_id} parent to {parent_id} would create a hierarchy cycle"
            )));
        }
    }
    let now = now.to_rfc3339();
    let changed = tx.execute("UPDATE contexts SET parent_id = ?1, updated_at = ?2 WHERE id = ?3", params![
        parent_id.map(|id| id.to_string()),
        now,
        context_id.to_string()
    ])?;
    if changed != 1 {
        return Err(StoreError::Conflict(format!("context {context_id} changed during parent update")));
    }
    insert_context_audit(&tx, audit, Some(&context_id), None, &now)?;
    tx.commit()?;
    Ok(())
}

#[expect(clippy::too_many_arguments, reason = "transactional lifecycle mutation needs target, state, principal, audit, and timestamp")]
fn set_context_lifecycle_conn(
    conn: &mut Connection,
    context_id: ContextId,
    lifecycle: ContextLifecycle,
    principal: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    validate_audit_actor(audit, principal)?;
    let tx = sqlite_write_tx(conn)?;
    require_mutable_owned_context(&tx, &context_id, principal)?;
    let now = now.to_rfc3339();
    let changed = tx.execute("UPDATE contexts SET lifecycle = ?1, updated_at = ?2 WHERE id = ?3", params![
        lifecycle.to_string(),
        now,
        context_id.to_string()
    ])?;
    if changed != 1 {
        return Err(StoreError::Conflict(format!("context {context_id} changed during lifecycle update")));
    }
    insert_context_audit(&tx, audit, Some(&context_id), None, &now)?;
    tx.commit()?;
    Ok(())
}

#[expect(clippy::too_many_arguments, reason = "transactional grant mutation needs context, grantee, principal, audit, and timestamp")]
fn grant_context_use_conn(
    conn: &mut Connection,
    context_id: ContextId,
    grantee_principal: &str,
    principal: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    validate_audit_actor(audit, principal)?;
    let grantee_principal = normalize_explicit_grantee(grantee_principal)?;
    let tx = sqlite_write_tx(conn)?;
    require_mutable_owned_context(&tx, &context_id, principal)?;
    let now = now.to_rfc3339();
    let _changed = tx.execute(
        "INSERT INTO context_grants (context_id, grantee_principal, granted_by, created_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(context_id, grantee_principal) DO UPDATE SET
            granted_by = excluded.granted_by,
            created_at = excluded.created_at",
        params![context_id.to_string(), grantee_principal, principal, now],
    )?;
    insert_context_audit(&tx, audit, Some(&context_id), None, &now)?;
    tx.commit()?;
    Ok(())
}

#[expect(clippy::too_many_arguments, reason = "transactional membership replacement needs memberships, principal, audit, and timestamp")]
fn replace_memory_contexts_conn(
    conn: &mut Connection,
    memory_id: MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
) -> Result<WriteOutcome, StoreError> {
    validate_audit_actor(audit, principal)?;
    let unique = context_ids.iter().copied().collect::<HashSet<_>>();
    if unique.len() != context_ids.len() {
        return Err(StoreError::Conflict("memory context memberships must be unique".into()));
    }
    let tx = sqlite_write_tx(conn)?;
    let Some(memory) = fetch_memory_by_id(&tx, &memory_id.to_string())? else {
        return Ok(WriteOutcome::NotFound);
    };
    if !write_access_allowed(&memory.provenance, &memory.access_policy, principal) {
        return Ok(WriteOutcome::Denied);
    }
    let mut statement = tx.prepare("SELECT context_id FROM memory_contexts WHERE memory_id = ?1")?;
    let preserved_context_ids = statement
        .query_map([memory_id.to_string()], |row| row.get::<_, String>(0))?
        .map(|result| result.and_then(|id| ContextId::from_str(&id).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))))
        .collect::<Result<HashSet<_>, _>>()?;
    drop(statement);
    for context_id in context_ids {
        let require_active = !preserved_context_ids.contains(context_id);
        if !context_use_allowed(&tx, context_id, principal, require_active)? {
            return Err(StoreError::Conflict(format!(
                "context {context_id} is unavailable, archived, or not granted to principal {principal:?}"
            )));
        }
    }

    let now = now.to_rfc3339();
    let _removed = tx.execute("DELETE FROM memory_contexts WHERE memory_id = ?1", [memory_id.to_string()])?;
    for (ordinal, context_id) in context_ids.iter().enumerate() {
        let ordinal = sqlite_usize(ordinal, "context membership ordinal")?;
        let _inserted = tx.execute(
            "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![memory_id.to_string(), context_id.to_string(), ordinal, now],
        )?;
    }
    let primary_key = if let Some(primary) = context_ids.first() {
        tx.query_row("SELECT context_key FROM contexts WHERE id = ?1", [primary.to_string()], |row| row.get::<_, String>(0))?
    } else {
        UNRESOLVED_SCOPE.into()
    };
    let _updated_metadata = tx.execute("UPDATE memory_metadata SET scope_key = ?1, updated_at = ?2 WHERE memory_id = ?3", params![
        primary_key,
        now,
        memory_id.to_string()
    ])?;
    let _updated_memory = tx.execute(
        "UPDATE memories
         SET provenance = json_set(provenance, '$.source_conversation', ?1),
             record_revision = record_revision + 1
         WHERE id = ?2",
        params![primary_key, memory_id.to_string()],
    )?;
    insert_context_audit(&tx, audit, None, Some(&memory_id), &now)?;
    tx.commit()?;
    Ok(WriteOutcome::Applied)
}

impl ContextReader for SqliteStore {
    async fn get_context(&self, id: &ContextId, principal: &str) -> Result<Option<ContextDefinition>, StoreError> {
        let owned_id = *id;
        let principal = principal.to_owned();
        self.with_conn(move |conn| fetch_context_authorized(conn, &owned_id, &principal)).await
    }

    async fn list_contexts(&self, principal: &str, include_archived: bool, offset: usize, limit: usize) -> Result<Vec<ContextDefinition>, StoreError> {
        let principal = principal.to_owned();
        let limit = sqlite_usize(limit.min(MAX_CONTEXT_PAGE_SIZE), "context page limit")?;
        let offset = sqlite_usize(offset, "context page offset")?;
        self.with_conn(move |conn| {
            let lifecycle_clause = if include_archived { "" } else { "AND context_row.lifecycle = 'active'" };
            let sql = format!(
                "SELECT {CONTEXT_COLUMNS}
                 FROM contexts AS context_row
                 WHERE {}
                   {lifecycle_clause}
                 ORDER BY context_row.kind, context_row.normalized_key, context_row.id
                 LIMIT ?2 OFFSET ?3",
                context_visible_sql("context_row")
            );
            let mut statement = conn.prepare(&sql)?;
            let mut rows = statement.query(params![principal, limit, offset])?;
            let mut contexts = Vec::new();
            while let Some(row) = rows.next()? {
                contexts.push(parse_context_row(read_context_row(row)?)?);
            }
            Ok(contexts)
        })
        .await
    }

    async fn list_context_records(&self, principal: &str, include_archived: bool, offset: usize, limit: usize) -> Result<Vec<ContextRecord>, StoreError> {
        let principal = principal.to_owned();
        let limit = sqlite_usize(limit.min(MAX_CONTEXT_PAGE_SIZE), "context page limit")?;
        let offset = sqlite_usize(offset, "context page offset")?;
        self.with_conn(move |conn| {
            let lifecycle_clause = if include_archived { "" } else { "AND context_row.lifecycle = 'active'" };
            let sql = format!(
                "SELECT {CONTEXT_COLUMNS}
                 FROM contexts AS context_row
                 WHERE {}
                   {lifecycle_clause}
                 ORDER BY context_row.kind, context_row.normalized_key, context_row.id
                 LIMIT ?2 OFFSET ?3",
                context_visible_sql("context_row")
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(params![principal, limit, offset], read_context_row)?.collect::<Result<Vec<_>, _>>()?;
            let contexts = rows.into_iter().map(parse_context_row).collect::<Result<Vec<_>, _>>()?;
            fetch_sqlite_context_records(conn, contexts)
        })
        .await
    }

    #[expect(clippy::too_many_lines, reason = "indexed exact lookup keeps each locator's SQL and bindings explicit")]
    async fn find_context_records(&self, principal: &str, include_archived: bool, lookup: &ContextExactLookup) -> Result<Vec<ContextRecord>, StoreError> {
        let principal = principal.to_owned();
        let lookup = lookup.clone();
        self.with_conn(move |conn| {
            let lifecycle_clause = if include_archived { "" } else { "AND context_row.lifecycle = 'active'" };
            let rows = match lookup {
                ContextExactLookup::Id(id) => {
                    let sql = format!(
                        "SELECT {CONTEXT_COLUMNS}
                         FROM contexts AS context_row
                         WHERE {}
                           AND context_row.id = ?2
                           {lifecycle_clause}
                         LIMIT 6",
                        context_visible_sql("context_row")
                    );
                    let mut statement = conn.prepare(&sql)?;
                    statement.query_map(params![principal, id.to_string()], read_context_row)?.collect::<Result<Vec<_>, _>>()?
                }
                ContextExactLookup::Key { kind: Some(kind), normalized_key } => {
                    let sql = format!(
                        "WITH candidate_ids(id) AS (
                             SELECT context_row.id
                             FROM contexts AS context_row
                             WHERE context_row.kind = ?2
                               AND context_row.normalized_key = ?3
                             UNION
                             SELECT alias_row.context_id
                             FROM context_aliases AS alias_row INDEXED BY idx_context_aliases_lookup
                             JOIN contexts AS candidate ON candidate.id = alias_row.context_id
                             WHERE candidate.kind = ?2
                               AND alias_row.normalized_alias = ?3
                         )
                         SELECT {CONTEXT_COLUMNS}
                         FROM contexts AS context_row
                         JOIN candidate_ids AS candidate ON candidate.id = context_row.id
                         WHERE {}
                           {lifecycle_clause}
                         ORDER BY context_row.normalized_key, context_row.id
                         LIMIT 6",
                        context_visible_sql("context_row")
                    );
                    let mut statement = conn.prepare(&sql)?;
                    statement
                        .query_map(params![principal, kind.as_str(), normalized_key], read_context_row)?
                        .collect::<Result<Vec<_>, _>>()?
                }
                ContextExactLookup::Key { kind: None, normalized_key } => {
                    let sql = format!(
                        "WITH candidate_ids(id) AS (
                             SELECT context_row.id
                             FROM contexts AS context_row
                             WHERE context_row.normalized_key = ?2
                             UNION
                             SELECT alias_row.context_id
                             FROM context_aliases AS alias_row
                             WHERE alias_row.normalized_alias = ?2
                         )
                         SELECT {CONTEXT_COLUMNS}
                         FROM contexts AS context_row
                         JOIN candidate_ids AS candidate ON candidate.id = context_row.id
                         WHERE {}
                           {lifecycle_clause}
                         ORDER BY context_row.kind, context_row.normalized_key, context_row.id
                         LIMIT 6",
                        context_visible_sql("context_row")
                    );
                    let mut statement = conn.prepare(&sql)?;
                    statement.query_map(params![principal, normalized_key], read_context_row)?.collect::<Result<Vec<_>, _>>()?
                }
                ContextExactLookup::Identity { kind, identity } => {
                    let sql = format!(
                        "WITH candidate_ids(id) AS (
                             SELECT identity_row.context_id
                             FROM context_identities AS identity_row
                             WHERE identity_row.kind = ?2
                               AND identity_row.scheme = ?3
                               AND identity_row.namespace = ?4
                               AND identity_row.fingerprint = ?5
                         )
                         SELECT {CONTEXT_COLUMNS}
                         FROM contexts AS context_row
                         JOIN candidate_ids AS candidate ON candidate.id = context_row.id
                         WHERE {}
                           AND context_row.kind = ?2
                           {lifecycle_clause}
                         ORDER BY context_row.normalized_key, context_row.id
                         LIMIT 6",
                        context_visible_sql("context_row")
                    );
                    let mut statement = conn.prepare(&sql)?;
                    statement
                        .query_map(
                            params![principal, kind.as_str(), identity.scheme, identity.namespace.unwrap_or_default(), identity.fingerprint],
                            read_context_row,
                        )?
                        .collect::<Result<Vec<_>, _>>()?
                }
            };
            rows.into_iter()
                .map(parse_context_row)
                .map(|result| result.and_then(|context| fetch_sqlite_context_record(conn, context)))
                .collect()
        })
        .await
    }

    #[expect(clippy::excessive_nesting, reason = "SQLite hierarchy expansion validates direct selections inside one connection closure")]
    async fn expand_context_selection(&self, context_ids: &[ContextId], principal: &str, include_descendants: bool) -> Result<Vec<ContextDefinition>, StoreError> {
        let context_ids = context_ids.to_vec();
        let principal = principal.to_owned();
        self.with_conn(move |conn| {
            for context_id in &context_ids {
                let context = fetch_context_authorized(conn, context_id, &principal)?
                    .ok_or_else(|| StoreError::NotFound(format!("context not found, archived, or not granted: {context_id}")))?;
                if context.lifecycle != ContextLifecycle::Active {
                    return Err(StoreError::Conflict(format!("context {context_id} is archived and cannot be selected")));
                }
            }
            let selected_json =
                serde_json::to_string(&context_ids.iter().map(ToString::to_string).collect::<Vec<_>>()).map_err(|error| StoreError::Serialization(Box::new(error)))?;
            let sql = format!(
                "WITH RECURSIVE selected(id) AS (
                     SELECT CAST(value AS TEXT) FROM json_each(?2)
                 ),
                 descendants(id) AS (
                     SELECT id FROM selected
                     UNION
                     SELECT child.id
                     FROM contexts AS child
                     JOIN descendants AS selected_parent ON child.parent_id = selected_parent.id
                     WHERE ?3
                       AND {}
                 ),
                 effective(id) AS (
                     SELECT id FROM descendants
                     UNION
                     SELECT parent.id
                     FROM contexts AS parent
                     JOIN contexts AS child ON child.parent_id = parent.id
                     JOIN effective AS effective_child ON effective_child.id = child.id
                     WHERE {}
                 )
                 SELECT {CONTEXT_COLUMNS}
                 FROM contexts AS context_row
                 JOIN effective AS selected_context ON selected_context.id = context_row.id
                 ORDER BY context_row.kind, context_row.normalized_key, context_row.id",
                context_visible_sql("child"),
                context_visible_sql("parent")
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement
                .query_map(params![principal, selected_json, include_descendants], read_context_row)?
                .collect::<Result<Vec<_>, _>>()?;
            let definitions = rows.into_iter().map(parse_context_row).collect::<Result<Vec<_>, _>>()?;
            expand_context_definitions(&definitions, &context_ids, include_descendants)
        })
        .await
    }

    async fn get_memory_contexts(&self, memory_id: &MemoryId, principal: &str) -> Result<Vec<MemoryContext>, StoreError> {
        let owned_memory_id = *memory_id;
        let principal = principal.to_owned();
        let now = self.clock_now();
        self.with_conn(move |conn| {
            let Some(memory) = fetch_memory_by_id(conn, &owned_memory_id.to_string())? else {
                return Err(StoreError::NotFound(format!("memory not found: {owned_memory_id}")));
            };
            if memory.expires_at.is_some_and(|expires_at| now >= expires_at) || memory.check_access_level(Some(&principal)) == AccessLevel::Denied {
                return Err(StoreError::NotFound(format!("memory not found: {owned_memory_id}")));
            }
            let sql = format!(
                "SELECT {CONTEXT_COLUMNS}, membership.ordinal
                 FROM memory_contexts AS membership
                 JOIN contexts AS context_row ON context_row.id = membership.context_id
                 WHERE membership.memory_id = ?2
                   AND {}
                 ORDER BY membership.ordinal",
                context_visible_sql("context_row")
            );
            let mut statement = conn.prepare(&sql)?;
            let mut rows = statement.query(params![principal, owned_memory_id.to_string()])?;
            let mut memberships = Vec::new();
            while let Some(row) = rows.next()? {
                let ordinal: i64 = row.get(12)?;
                memberships.push(MemoryContext {
                    memory_id: owned_memory_id,
                    context: parse_context_row(read_context_row(row)?)?,
                    ordinal: u32::try_from(ordinal).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                });
            }
            Ok(memberships)
        })
        .await
    }

    async fn get_memory_contexts_batch(&self, memory_ids: &[MemoryId], principal: &str) -> Result<HashMap<MemoryId, Vec<MemoryContext>>, StoreError> {
        if memory_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids_json = serde_json::to_string(&memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        let principal = principal.to_owned();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            let sql = format!(
                "SELECT {CONTEXT_COLUMNS}, membership.ordinal, membership.memory_id,
                        memory_row.provenance, memory_row.access_policy
                 FROM memory_contexts AS membership
                 JOIN memories AS memory_row ON memory_row.id = membership.memory_id
                 JOIN contexts AS context_row ON context_row.id = membership.context_id
                 WHERE membership.memory_id IN (SELECT value FROM json_each(?2))
                   AND (memory_row.expires_at IS NULL OR memory_row.expires_at > ?3)
                   AND {}
                 ORDER BY membership.memory_id, membership.ordinal",
                context_visible_sql("context_row")
            );
            let mut statement = conn.prepare(&sql)?;
            let mut rows = statement.query(params![principal, ids_json, now])?;
            let mut memberships = HashMap::<MemoryId, Vec<MemoryContext>>::new();
            while let Some(row) = rows.next()? {
                append_memory_context(&mut memberships, parse_sqlite_memory_context_batch_row(row, &principal)?);
            }
            Ok(memberships)
        })
        .await
    }

    async fn get_memory_context_presence_batch(&self, memory_ids: &[MemoryId], principal: &str) -> Result<MemoryContextPresence, StoreError> {
        if memory_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let ids_json = serde_json::to_string(&memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        let principal = principal.to_owned();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT DISTINCT memory_row.id, memory_row.provenance, memory_row.access_policy
                 FROM memories AS memory_row
                 JOIN memory_contexts AS membership ON membership.memory_id = memory_row.id
                 WHERE memory_row.id IN (SELECT value FROM json_each(?1))
                   AND (memory_row.expires_at IS NULL OR memory_row.expires_at > ?2)",
            )?;
            let rows = statement.query_map(params![ids_json, now], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?;
            let mut present = HashSet::new();
            for row in rows {
                present.extend(readable_membership_memory_id(row?, &principal)?);
            }
            Ok(present)
        })
        .await
    }

    async fn count_memory_contexts_for_write(&self, memory_id: &MemoryId, principal: &str) -> Result<Option<usize>, StoreError> {
        let owned_memory_id = *memory_id;
        let principal = principal.to_owned();
        self.with_conn(move |conn| {
            let Some(memory) = fetch_memory_by_id(conn, &owned_memory_id.to_string())? else {
                return Ok(None);
            };
            if !write_access_allowed(&memory.provenance, &memory.access_policy, &principal) {
                return Ok(None);
            }
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_contexts WHERE memory_id = ?1", [owned_memory_id.to_string()], |row| row.get(0))?;
            let count = usize::try_from(count).map_err(|error| StoreError::Serialization(Box::new(error)))?;
            Ok(Some(count))
        })
        .await
    }

    async fn query_context_audit(&self, context_id: &ContextId, principal: &str, limit: usize) -> Result<Vec<ContextAuditEvent>, StoreError> {
        let owned_context_id = *context_id;
        let principal = principal.to_owned();
        let limit = sqlite_usize(limit.min(MAX_CONTEXT_AUDIT_PAGE_SIZE), "context audit limit")?;
        self.with_conn(move |conn| {
            let Some(context) = fetch_context_authorized(conn, &owned_context_id, &principal)? else {
                return Err(StoreError::NotFound(format!("context not found: {owned_context_id}")));
            };
            if context.owner_principal != principal {
                return Ok(Vec::new());
            }
            let mut statement = conn.prepare(
                "SELECT id, actor_principal, action, context_id, memory_id, timestamp, details
                 FROM context_audit_events
                 WHERE context_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )?;
            let mut rows = statement.query(params![owned_context_id.to_string(), limit])?;
            let mut events = Vec::new();
            while let Some(row) = rows.next()? {
                let stored_context_id: Option<String> = row.get(3)?;
                let stored_memory_id: Option<String> = row.get(4)?;
                let details: Option<String> = row.get(6)?;
                events.push(ContextAuditEvent {
                    id: row.get(0)?,
                    actor_principal: row.get(1)?,
                    action: row.get(2)?,
                    context_id: stored_context_id
                        .map(|id| ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
                        .transpose()?,
                    memory_id: stored_memory_id
                        .map(|id| MemoryId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
                        .transpose()?,
                    timestamp: parse_timestamp(&row.get::<_, String>(5)?, "context_audit_events.timestamp")?,
                    details: details.map(|value| serde_json::from_str(&value)).transpose()?,
                });
            }
            Ok(events)
        })
        .await
    }

    async fn list_context_kinds(&self) -> Result<Vec<ContextKindDefinition>, StoreError> {
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT kind, display_name, builtin, enabled, created_at, updated_at
                 FROM context_kinds
                 ORDER BY builtin DESC, kind",
            )?;
            let mut rows = statement.query([])?;
            let mut kinds = Vec::new();
            while let Some(row) = rows.next()? {
                kinds.push(ContextKindDefinition {
                    kind: ContextKind::new(row.get::<_, String>(0)?).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    display_name: row.get(1)?,
                    builtin: row.get(2)?,
                    enabled: row.get(3)?,
                    created_at: parse_timestamp(&row.get::<_, String>(4)?, "context_kinds.created_at")?,
                    updated_at: parse_timestamp(&row.get::<_, String>(5)?, "context_kinds.updated_at")?,
                });
            }
            Ok(kinds)
        })
        .await
    }

    async fn list_context_kind_policies(&self, principal: &str) -> Result<Vec<ContextKindPolicyRecord>, StoreError> {
        let principal = principal.to_owned();
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT layer, principal, kind, policy_json, updated_at
                 FROM context_kind_policies
                 WHERE layer = 'operator' OR (layer = 'principal' AND principal = ?1)
                 ORDER BY layer, kind",
            )?;
            let mut rows = statement.query([principal])?;
            let mut policies = Vec::new();
            while let Some(row) = rows.next()? {
                let policy_json: String = row.get(3)?;
                policies.push(ContextKindPolicyRecord {
                    layer: ContextPolicyLayer::from_str(&row.get::<_, String>(0)?).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    principal: row.get(1)?,
                    kind: ContextKind::new(row.get::<_, String>(2)?).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    policy: serde_json::from_str(&policy_json)?,
                    updated_at: parse_timestamp(&row.get::<_, String>(4)?, "context_kind_policies.updated_at")?,
                });
            }
            Ok(policies)
        })
        .await
    }

    async fn list_context_anchor_policies(&self, principal: &str) -> Result<Vec<ContextAnchorPolicyRecord>, StoreError> {
        let principal = principal.to_owned();
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT override_row.anchor_context_id, override_row.principal,
                        override_row.policy_json, override_row.updated_at
                 FROM context_anchor_overrides AS override_row
                 JOIN contexts AS context_row ON context_row.id = override_row.anchor_context_id
                 WHERE override_row.principal = ?1
                   AND context_row.lifecycle = 'active'
                   AND (
                       context_row.owner_principal = ?1 OR EXISTS (
                           SELECT 1 FROM context_grants AS grant_row
                           WHERE grant_row.context_id = context_row.id
                             AND grant_row.grantee_principal IN (?1, ?2)
                       )
                   )
                 ORDER BY override_row.anchor_context_id",
            )?;
            let mut rows = statement.query(params![principal, LEGACY_ALL_PRINCIPALS_GRANT])?;
            let mut policies = Vec::new();
            while let Some(row) = rows.next()? {
                let context_id: String = row.get(0)?;
                let policy_json: String = row.get(2)?;
                policies.push(ContextAnchorPolicyRecord {
                    anchor_context_id: ContextId::from_str(&context_id).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    principal: row.get(1)?,
                    policy: serde_json::from_str(&policy_json)?,
                    updated_at: parse_timestamp(&row.get::<_, String>(3)?, "context_anchor_overrides.updated_at")?,
                });
            }
            Ok(policies)
        })
        .await
    }

    async fn list_context_grants(&self, context_id: &ContextId, principal: &str) -> Result<Vec<ContextGrant>, StoreError> {
        let owned_context_id = *context_id;
        let principal = principal.to_owned();
        self.with_conn(move |conn| {
            let Some(context) = fetch_context_authorized(conn, &owned_context_id, &principal)? else {
                return Err(StoreError::NotFound(format!("context not found: {owned_context_id}")));
            };
            if context.owner_principal != principal {
                return Ok(Vec::new());
            }
            let mut statement = conn.prepare(
                "SELECT grantee_principal, granted_by, created_at
                 FROM context_grants
                 WHERE context_id = ?1
                 ORDER BY grantee_principal",
            )?;
            let mut rows = statement.query([owned_context_id.to_string()])?;
            let mut grants = Vec::new();
            while let Some(row) = rows.next()? {
                grants.push(ContextGrant {
                    context_id: owned_context_id,
                    grantee_principal: row.get(0)?,
                    granted_by: row.get(1)?,
                    created_at: parse_timestamp(&row.get::<_, String>(2)?, "context_grants.created_at")?,
                });
            }
            Ok(grants)
        })
        .await
    }
}

#[expect(
    clippy::excessive_nesting,
    reason = "hierarchy expansion performs cycle checks while walking ancestors and optional descendants"
)]
fn expand_context_definitions(definitions: &[ContextDefinition], direct_ids: &[ContextId], include_descendants: bool) -> Result<Vec<ContextDefinition>, StoreError> {
    let by_id = definitions.iter().cloned().map(|context| (context.id, context)).collect::<HashMap<_, _>>();
    let mut result = Vec::new();
    let mut included = HashSet::new();
    for direct_id in direct_ids {
        let direct = by_id
            .get(direct_id)
            .ok_or_else(|| StoreError::NotFound(format!("context not found, archived, or not granted: {direct_id}")))?;
        if direct.lifecycle != ContextLifecycle::Active {
            return Err(StoreError::Conflict(format!("context {direct_id} is archived and cannot be selected")));
        }
        append_context_once(&mut result, &mut included, direct);
        let mut cursor = direct.parent_id;
        let mut ancestors = HashSet::new();
        while let Some(parent_id) = cursor {
            if !ancestors.insert(parent_id) {
                return Err(StoreError::Conflict("context hierarchy contains a cycle".into()));
            }
            let parent = by_id
                .get(&parent_id)
                .ok_or_else(|| StoreError::Conflict(format!("context {direct_id} references unavailable parent {parent_id}")))?;
            if parent.lifecycle == ContextLifecycle::Active {
                append_context_once(&mut result, &mut included, parent);
            }
            cursor = parent.parent_id;
        }
    }
    if include_descendants {
        let anchors = direct_ids.iter().copied().collect::<HashSet<_>>();
        for candidate in definitions.iter().filter(|context| context.lifecycle == ContextLifecycle::Active) {
            let mut cursor = candidate.parent_id;
            let mut visited = HashSet::new();
            while let Some(parent_id) = cursor {
                if !visited.insert(parent_id) {
                    return Err(StoreError::Conflict("context hierarchy contains a cycle".into()));
                }
                if anchors.contains(&parent_id) {
                    append_context_once(&mut result, &mut included, candidate);
                    break;
                }
                cursor = by_id.get(&parent_id).and_then(|parent| parent.parent_id);
            }
        }
    }
    Ok(result)
}

fn append_context_once(result: &mut Vec<ContextDefinition>, included: &mut HashSet<ContextId>, context: &ContextDefinition) {
    if included.insert(context.id) {
        result.push(context.clone());
    }
}

fn validate_policy_default_sqlite(tx: &Transaction<'_>, kind: &ContextKind, policy: &ContextKindPolicy, principal: &str) -> Result<(), StoreError> {
    let kind_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM context_kinds WHERE kind = ?1)", [kind.as_str()], |row| row.get(0))?;
    if !kind_exists {
        return Err(StoreError::Conflict(format!("unknown context kind {:?}", kind.as_str())));
    }
    let Some(default_id) = policy.default_context_id else {
        return Ok(());
    };
    let default_kind: Option<String> = tx
        .query_row("SELECT kind FROM contexts WHERE id = ?1 AND lifecycle = 'active'", [default_id.to_string()], |row| {
            row.get(0)
        })
        .optional()?;
    if default_kind.as_deref() != Some(kind.as_str()) {
        return Err(StoreError::Conflict("policy default_context_id must reference an active context of the same kind".into()));
    }
    if !principal.is_empty() && !context_use_allowed(tx, &default_id, principal, true)? {
        return Err(StoreError::Conflict("principal policy default_context_id is not authorized for that principal".into()));
    }
    Ok(())
}

impl ContextWriter for SqliteStore {
    async fn create_context(&self, draft: &ContextCreateDraft, audit: &ContextAuditDraft) -> Result<ContextDefinition, StoreError> {
        let draft = draft.clone();
        let audit = audit.clone();
        let now = self.clock_now();
        self.with_conn(move |conn| create_context_conn(conn, &draft, &audit, now)).await
    }

    async fn set_context_parent(&self, context_id: &ContextId, parent_id: Option<&ContextId>, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let owned_context_id = *context_id;
        let parent_id = parent_id.copied();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now();
        self.with_conn(move |conn| set_context_parent_conn(conn, owned_context_id, parent_id, &principal, &audit, now))
            .await
    }

    async fn set_context_lifecycle(&self, context_id: &ContextId, lifecycle: ContextLifecycle, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let owned_context_id = *context_id;
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now();
        self.with_conn(move |conn| set_context_lifecycle_conn(conn, owned_context_id, lifecycle, &principal, &audit, now))
            .await
    }

    async fn grant_context_use(&self, context_id: &ContextId, grantee_principal: &str, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let owned_context_id = *context_id;
        let grantee_principal = grantee_principal.to_owned();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now();
        self.with_conn(move |conn| grant_context_use_conn(conn, owned_context_id, &grantee_principal, &principal, &audit, now))
            .await
    }

    async fn revoke_context_use(&self, context_id: &ContextId, grantee_principal: &str, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let owned_context_id = *context_id;
        let grantee_principal = grantee_principal.to_owned();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now();
        self.with_conn(move |conn| {
            validate_audit_actor(&audit, &principal)?;
            let grantee_principal = normalize_explicit_grantee(&grantee_principal)?;
            let tx = sqlite_write_tx(conn)?;
            require_mutable_owned_context(&tx, &owned_context_id, &principal)?;
            let _removed = tx.execute("DELETE FROM context_grants WHERE context_id = ?1 AND grantee_principal = ?2", params![
                owned_context_id.to_string(),
                grantee_principal
            ])?;
            insert_context_audit(&tx, &audit, Some(&owned_context_id), None, &now.to_rfc3339())?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn replace_context_grants(&self, context_id: &ContextId, grantee_principals: &[String], principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let owned_context_id = *context_id;
        let grantees = normalize_explicit_grantees(grantee_principals)?;
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            validate_audit_actor(&audit, &principal)?;
            let tx = sqlite_write_tx(conn)?;
            require_mutable_owned_context(&tx, &owned_context_id, &principal)?;
            let _removed = tx.execute("DELETE FROM context_grants WHERE context_id = ?1", [owned_context_id.to_string()])?;
            for grantee in grantees {
                let _inserted = tx.execute(
                    "INSERT INTO context_grants (context_id, grantee_principal, granted_by, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![owned_context_id.to_string(), grantee, principal, now],
                )?;
            }
            insert_context_audit(&tx, &audit, Some(&owned_context_id), None, &now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "definition replacement atomically validates and replaces aliases, identities, hints, and audit"
    )]
    #[expect(clippy::excessive_nesting, reason = "each complete replacement set is normalized before the transaction begins")]
    async fn update_context_definition(&self, context_id: &ContextId, patch: &ContextDefinitionPatch, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let owned_context_id = *context_id;
        let patch = patch.clone();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            validate_audit_actor(&audit, &principal)?;
            validate_definition_patch_surfaces(&patch)?;
            let display_name = patch.display_name.trim();
            let mut aliases = Vec::new();
            let mut normalized_aliases = HashSet::new();
            for alias in &patch.aliases {
                let alias = alias.trim();
                let normalized = normalize_context_key(alias);
                if alias.is_empty() || normalized.is_empty() || !normalized_aliases.insert(normalized.clone()) {
                    return Err(StoreError::Conflict("context aliases must be non-empty and unique after normalization".into()));
                }
                aliases.push((alias.to_owned(), normalized));
            }
            let mut identities = HashSet::new();
            for identity in &patch.identities {
                validate_identity(identity)?;
                if !identities.insert((identity.scheme.clone(), identity.namespace.clone(), identity.fingerprint.clone())) {
                    return Err(StoreError::Conflict("context identities must be unique".into()));
                }
            }
            let mut hints = Vec::new();
            let mut normalized_hints = HashSet::new();
            for hint in &patch.resolver_hints {
                let hint = hint.trim();
                let normalized = normalize_context_key(hint);
                if hint.is_empty() || normalized.is_empty() || !normalized_hints.insert(normalized.clone()) {
                    return Err(StoreError::Conflict("context resolver hints must be non-empty and unique after normalization".into()));
                }
                hints.push((hint.to_owned(), normalized));
            }

            let tx = sqlite_write_tx(conn)?;
            require_mutable_owned_context(&tx, &owned_context_id, &principal)?;
            let (owner, kind): (String, String) = tx.query_row("SELECT owner_principal, kind FROM contexts WHERE id = ?1", [owned_context_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            let _updated = tx.execute(
                "UPDATE contexts
                 SET display_name = ?1, description = ?2, guidance = ?3, updated_at = ?4
                 WHERE id = ?5",
                params![
                    display_name,
                    patch.description.as_deref().map(str::trim).filter(|value| !value.is_empty()),
                    patch.guidance.as_deref().map(str::trim).filter(|value| !value.is_empty()),
                    now,
                    owned_context_id.to_string(),
                ],
            )?;
            let _aliases_removed = tx.execute("DELETE FROM context_aliases WHERE context_id = ?1", [owned_context_id.to_string()])?;
            for (alias, normalized) in aliases {
                let _inserted = tx.execute(
                    "INSERT INTO context_aliases (context_id, alias, normalized_alias, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![owned_context_id.to_string(), alias, normalized, now],
                )?;
            }
            let _identities_removed = tx.execute("DELETE FROM context_identities WHERE context_id = ?1", [owned_context_id.to_string()])?;
            for identity in &patch.identities {
                let _inserted = tx.execute(
                    "INSERT INTO context_identities (
                        context_id, owner_principal, kind, scheme, namespace,
                        fingerprint, redacted_label, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        owned_context_id.to_string(),
                        owner,
                        kind,
                        identity.scheme,
                        identity.namespace.as_deref().unwrap_or_default(),
                        identity.fingerprint,
                        identity.redacted_label,
                        now,
                    ],
                )?;
            }
            let _hints_removed = tx.execute("DELETE FROM context_resolver_hints WHERE context_id = ?1", [owned_context_id.to_string()])?;
            for (hint, normalized) in hints {
                let _inserted = tx.execute(
                    "INSERT INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![owned_context_id.to_string(), hint, normalized, now],
                )?;
            }
            insert_context_audit(&tx, &audit, Some(&owned_context_id), None, &now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn upsert_context_kind(&self, draft: &ContextKindDraft, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let draft = draft.clone();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            validate_audit_actor(&audit, &principal)?;
            if principal != OPERATOR_PRINCIPAL {
                return Err(StoreError::Conflict(format!("context kind mutation requires principal {OPERATOR_PRINCIPAL:?}")));
            }
            let display_name = draft.display_name.trim();
            if display_name.is_empty() {
                return Err(StoreError::Conflict("context kind display name cannot be blank".into()));
            }
            let tx = sqlite_write_tx(conn)?;
            let _changed = tx.execute(
                "INSERT INTO context_kinds (
                    kind, display_name, builtin, enabled, created_at, updated_at
                 ) VALUES (?1, ?2, 0, ?3, ?4, ?4)
                 ON CONFLICT(kind) DO UPDATE SET
                    display_name = excluded.display_name,
                    enabled = excluded.enabled,
                    updated_at = excluded.updated_at",
                params![draft.kind.as_str(), display_name, draft.enabled, now],
            )?;
            insert_context_audit(&tx, &audit, None, None, &now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    #[expect(clippy::excessive_nesting, reason = "layer-specific ownership checks precede one transactional policy upsert")]
    async fn upsert_context_kind_policy(&self, draft: &ContextKindPolicyDraft, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let draft = draft.clone();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            validate_audit_actor(&audit, &principal)?;
            draft.policy.validate().map_err(StoreError::Conflict)?;
            let stored_principal = match draft.layer {
                ContextPolicyLayer::Operator => {
                    if !draft.principal.is_empty() {
                        return Err(StoreError::Conflict("operator policy principal must be empty".into()));
                    }
                    if principal != OPERATOR_PRINCIPAL {
                        return Err(StoreError::Conflict(format!("operator policy mutation requires principal {OPERATOR_PRINCIPAL:?}")));
                    }
                    ""
                }
                ContextPolicyLayer::Principal => {
                    if draft.principal != principal {
                        return Err(StoreError::Conflict("a principal policy may only customize its own principal".into()));
                    }
                    principal.as_str()
                }
            };
            let tx = sqlite_write_tx(conn)?;
            validate_policy_default_sqlite(&tx, &draft.kind, &draft.policy, stored_principal)?;
            let policy_json = serde_json::to_string(&draft.policy)?;
            let _changed = tx.execute(
                "INSERT INTO context_kind_policies (
                    layer, principal, kind, policy_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(layer, principal, kind) DO UPDATE SET
                    policy_json = excluded.policy_json,
                    updated_at = excluded.updated_at",
                params![draft.layer.to_string(), stored_principal, draft.kind.as_str(), policy_json, now],
            )?;
            insert_context_audit(&tx, &audit, None, None, &now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn upsert_context_anchor_policy(&self, draft: &ContextAnchorPolicyDraft, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        let draft = draft.clone();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            validate_audit_actor(&audit, &principal)?;
            if draft.principal != principal {
                return Err(StoreError::Conflict("an anchor policy may only customize its own principal".into()));
            }
            draft.policy.validate().map_err(StoreError::Conflict)?;
            let tx = sqlite_write_tx(conn)?;
            if !context_use_allowed(&tx, &draft.anchor_context_id, &principal, true)? {
                return Err(StoreError::Conflict("anchor context is unavailable, archived, or not granted".into()));
            }
            for (kind, policy) in &draft.policy.kinds {
                let kind = ContextKind::new(kind.clone()).map_err(|error| StoreError::Serialization(Box::new(error)))?;
                validate_policy_default_sqlite(&tx, &kind, policy, &principal)?;
            }
            let policy_json = serde_json::to_string(&draft.policy)?;
            let _changed = tx.execute(
                "INSERT INTO context_anchor_overrides (
                    anchor_context_id, principal, policy_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(anchor_context_id, principal) DO UPDATE SET
                    policy_json = excluded.policy_json,
                    updated_at = excluded.updated_at",
                params![draft.anchor_context_id.to_string(), principal, policy_json, now],
            )?;
            insert_context_audit(&tx, &audit, Some(&draft.anchor_context_id), None, &now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn rollback_unreferenced_legacy_context(&self, context_id: &ContextId, principal: &str) -> Result<bool, StoreError> {
        let context_id = context_id.to_string();
        let principal = principal.to_owned();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let removed = tx.execute(
                "DELETE FROM contexts
                 WHERE id = ?1
                   AND owner_principal = ?2
                   AND kind = 'custom'
                   AND frozen = 0
                   AND NOT EXISTS (SELECT 1 FROM memory_contexts WHERE context_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM contexts WHERE parent_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM context_grants WHERE context_id = ?1)
                   AND NOT EXISTS (
                       SELECT 1 FROM context_relations
                       WHERE from_context_id = ?1 OR to_context_id = ?1
                   )
                   AND NOT EXISTS (SELECT 1 FROM context_aliases WHERE context_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM context_identities WHERE context_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM context_resolver_hints WHERE context_id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM context_anchor_overrides WHERE anchor_context_id = ?1)
                   AND NOT EXISTS (
                       SELECT 1
                       FROM context_kind_policies AS policy,
                            json_tree(policy.policy_json) AS field
                       WHERE field.key = 'default_context_id'
                         AND field.value = ?1
                   )
                   AND NOT EXISTS (
                       SELECT 1
                       FROM context_anchor_overrides AS policy,
                            json_tree(policy.policy_json) AS field
                       WHERE field.key = 'default_context_id'
                         AND field.value = ?1
                   )
                   AND (
                       SELECT COUNT(*) FROM context_audit_events
                       WHERE context_id = ?1
                   ) = 1
                   AND EXISTS (
                       SELECT 1 FROM context_audit_events
                       WHERE context_id = ?1
                         AND actor_principal = ?2
                         AND action = 'legacy_scope_context_created'
                         AND memory_id IS NULL
                   )",
                params![context_id, principal],
            )?;
            if removed == 1 {
                let _removed_audit = tx.execute(
                    "DELETE FROM context_audit_events
                     WHERE context_id = ?1
                       AND actor_principal = ?2
                       AND action = 'legacy_scope_context_created'
                       AND memory_id IS NULL",
                    params![context_id, principal],
                )?;
            }
            tx.commit()?;
            Ok(removed == 1)
        })
        .await
    }

    async fn replace_memory_contexts(&self, memory_id: &MemoryId, context_ids: &[ContextId], principal: &str, audit: &ContextAuditDraft) -> Result<WriteOutcome, StoreError> {
        let owned_memory_id = *memory_id;
        let context_ids = context_ids.to_vec();
        let principal = principal.to_owned();
        let audit = audit.clone();
        let now = self.clock_now();
        self.with_conn(move |conn| replace_memory_contexts_conn(conn, owned_memory_id, &context_ids, &principal, &audit, now))
            .await
    }
}

fn parse_postgres_context_row(row: &PgRow) -> Result<ContextDefinition, StoreError> {
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let parent_id: Option<String> = row.try_get("parent_id")?;
    let lifecycle: String = row.try_get("lifecycle")?;
    Ok(ContextDefinition {
        id: ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        kind: ContextKind::new(kind).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        key: row.try_get("context_key")?,
        display_name: row.try_get("display_name")?,
        description: row.try_get("description")?,
        owner_principal: row.try_get("owner_principal")?,
        guidance: row.try_get("guidance")?,
        parent_id: parent_id
            .map(|id| ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
            .transpose()?,
        lifecycle: ContextLifecycle::from_str(&lifecycle).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        frozen: row.try_get("frozen")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn fetch_postgres_context_record(store: &PostgresStore, context: ContextDefinition) -> Result<ContextRecord, StoreError> {
    let context_id = context.id.to_string();
    let aliases = query_scalar::<Postgres, String>("SELECT alias FROM context_aliases WHERE context_id = $1 ORDER BY normalized_alias")
        .bind(&context_id)
        .fetch_all(store.pool())
        .await?;
    let identities = query(
        "SELECT scheme, NULLIF(namespace, '') AS namespace, fingerprint, redacted_label
         FROM context_identities
         WHERE context_id = $1
         ORDER BY scheme, namespace, fingerprint",
    )
    .bind(&context_id)
    .fetch_all(store.pool())
    .await?
    .iter()
    .map(|row| {
        Ok(ContextIdentity {
            scheme: row.try_get("scheme")?,
            namespace: row.try_get("namespace")?,
            fingerprint: row.try_get("fingerprint")?,
            redacted_label: row.try_get("redacted_label")?,
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    let hints = query_scalar::<Postgres, String>("SELECT hint FROM context_resolver_hints WHERE context_id = $1 ORDER BY normalized_hint")
        .bind(&context_id)
        .fetch_all(store.pool())
        .await?;
    Ok(ContextRecord {
        context,
        aliases,
        identities,
        hints,
    })
}

async fn postgres_context_use_allowed(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    context_id: &ContextId,
    principal: &str,
    require_active: bool,
) -> Result<bool, StoreError> {
    let lifecycle_clause = if require_active { "AND context_row.lifecycle = 'active'" } else { "" };
    let sql = format!(
        "SELECT EXISTS(
            SELECT 1 FROM contexts AS context_row
            JOIN context_kinds AS kind_row ON kind_row.kind = context_row.kind
            WHERE context_row.id = $2
              AND kind_row.enabled
              {lifecycle_clause}
              AND (
                  context_row.owner_principal = $1 OR EXISTS (
                      SELECT 1 FROM context_grants AS grant_row
                      WHERE grant_row.context_id = context_row.id
                        AND grant_row.grantee_principal IN ($1, '{LEGACY_ALL_PRINCIPALS_GRANT}')
                  )
              )
         )"
    );
    Ok(query_scalar(AssertSqlSafe(sql.as_str()))
        .bind(principal)
        .bind(context_id.to_string())
        .fetch_one(&mut **tx)
        .await?)
}

async fn lock_postgres_contexts_for_membership(tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>, context_ids: &[ContextId]) -> Result<(), StoreError> {
    let mut context_ids = context_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    context_ids.sort_unstable();
    context_ids.dedup();
    if context_ids.is_empty() {
        return Ok(());
    }
    let _locked_ids = query_scalar::<Postgres, String>(
        "SELECT id
         FROM contexts
         WHERE id = ANY($1)
         ORDER BY id
         FOR KEY SHARE",
    )
    .bind(&context_ids)
    .fetch_all(&mut **tx)
    .await?;
    let _locked_kinds = query_scalar::<Postgres, String>(
        "SELECT kind_row.kind
         FROM context_kinds AS kind_row
         WHERE kind_row.kind IN (
             SELECT context_row.kind
             FROM contexts AS context_row
             WHERE context_row.id = ANY($1)
         )
         ORDER BY kind_row.kind
         FOR KEY SHARE",
    )
    .bind(&context_ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(())
}

async fn require_postgres_mutable_owned_context(tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>, context_id: &ContextId, principal: &str) -> Result<(), StoreError> {
    let row = query("SELECT owner_principal, frozen FROM contexts WHERE id = $1 FOR UPDATE")
        .bind(context_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Err(StoreError::NotFound(format!("context not found: {context_id}")));
    };
    let owner: String = row.try_get("owner_principal")?;
    let frozen: bool = row.try_get("frozen")?;
    if owner != principal {
        return Err(StoreError::Conflict(format!("principal {principal:?} does not own context {context_id}")));
    }
    if frozen {
        return Err(StoreError::Conflict(format!("context {context_id} is a frozen legacy compatibility context")));
    }
    Ok(())
}

async fn validate_policy_default_postgres(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    kind: &ContextKind,
    policy: &ContextKindPolicy,
    principal: &str,
) -> Result<(), StoreError> {
    let kind_exists: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM context_kinds WHERE kind = $1)")
        .bind(kind.as_str())
        .fetch_one(&mut **tx)
        .await?;
    if !kind_exists {
        return Err(StoreError::Conflict(format!("unknown context kind {:?}", kind.as_str())));
    }
    let Some(default_id) = policy.default_context_id else {
        return Ok(());
    };
    let default_kind: Option<String> = query_scalar("SELECT kind FROM contexts WHERE id = $1 AND lifecycle = 'active' FOR KEY SHARE")
        .bind(default_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    if default_kind.as_deref() != Some(kind.as_str()) {
        return Err(StoreError::Conflict("policy default_context_id must reference an active context of the same kind".into()));
    }
    if !principal.is_empty() && !postgres_context_use_allowed(tx, &default_id, principal, true).await? {
        return Err(StoreError::Conflict("principal policy default_context_id is not authorized for that principal".into()));
    }
    Ok(())
}

async fn insert_postgres_context_audit(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    audit: &ContextAuditDraft,
    context_id: Option<&ContextId>,
    memory_id: Option<&MemoryId>,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    let _inserted = query(
        "INSERT INTO context_audit_events (
            actor_principal, action, context_id, memory_id, timestamp, details
         ) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&audit.actor_principal)
    .bind(&audit.action)
    .bind(context_id.map(ToString::to_string))
    .bind(memory_id.map(ToString::to_string))
    .bind(now)
    .bind(audit.details.clone().map(Json))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "membership insertion validates authorization, compatibility cache, audit, and timestamp in one transaction"
)]
pub(crate) async fn insert_initial_memory_contexts_postgres(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    memory_id: &MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    compatibility_scope: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    insert_memory_contexts_postgres(tx, memory_id, context_ids, principal, compatibility_scope, audit, now, &HashSet::new()).await
}

#[expect(
    clippy::too_many_arguments,
    reason = "membership insertion validates authorization, compatibility cache, preserved inactive memberships, audit, and timestamp"
)]
async fn insert_memory_contexts_postgres(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    memory_id: &MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    compatibility_scope: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
    preserved_context_ids: &HashSet<ContextId>,
) -> Result<(), StoreError> {
    validate_audit_actor(audit, principal)?;
    let unique = context_ids.iter().copied().collect::<HashSet<_>>();
    if unique.len() != context_ids.len() {
        return Err(StoreError::Conflict("memory context memberships must be unique".into()));
    }
    lock_postgres_contexts_for_membership(tx, context_ids).await?;
    for context_id in context_ids {
        let require_active = !preserved_context_ids.contains(context_id);
        if !postgres_context_use_allowed(tx, context_id, principal, require_active).await? {
            return Err(StoreError::Conflict(format!(
                "context {context_id} is unavailable, archived, or not granted to principal {principal:?}"
            )));
        }
    }
    let expected_scope = if let Some(primary) = context_ids.first() {
        query_scalar::<Postgres, String>("SELECT context_key FROM contexts WHERE id = $1")
            .bind(primary.to_string())
            .fetch_one(&mut **tx)
            .await?
    } else {
        UNRESOLVED_SCOPE.to_owned()
    };
    if compatibility_scope != expected_scope {
        return Err(StoreError::Conflict(format!(
            "compatibility scope {compatibility_scope:?} does not match primary governed context {expected_scope:?}"
        )));
    }
    for (ordinal, context_id) in context_ids.iter().enumerate() {
        let ordinal = i64::try_from(ordinal).map_err(|error| StoreError::Conflict(format!("context membership ordinal exceeds BIGINT: {error}")))?;
        let _inserted = query(
            "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(memory_id.to_string())
        .bind(context_id.to_string())
        .bind(ordinal)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    insert_postgres_context_audit(tx, audit, None, Some(memory_id), now).await
}

#[expect(
    clippy::too_many_arguments,
    reason = "atomic membership replacement carries authorization, compatibility cache, audit, and timestamp"
)]
pub(crate) async fn replace_memory_contexts_postgres_tx(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    memory_id: &MemoryId,
    context_ids: &[ContextId],
    principal: &str,
    compatibility_scope: &str,
    audit: &ContextAuditDraft,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    let preserved_context_ids = query_scalar::<Postgres, String>("SELECT context_id FROM memory_contexts WHERE memory_id = $1")
        .bind(memory_id.to_string())
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|id| ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
        .collect::<Result<HashSet<_>, _>>()?;
    let _removed = query("DELETE FROM memory_contexts WHERE memory_id = $1")
        .bind(memory_id.to_string())
        .execute(&mut **tx)
        .await?;
    insert_memory_contexts_postgres(tx, memory_id, context_ids, principal, compatibility_scope, audit, now, &preserved_context_ids).await?;
    let _updated_metadata = query("UPDATE memory_metadata SET scope_key = $1, updated_at = $2 WHERE memory_id = $3")
        .bind(compatibility_scope)
        .bind(now)
        .bind(memory_id.to_string())
        .execute(&mut **tx)
        .await?;
    let _updated_memory = query(
        "UPDATE memories
         SET provenance = jsonb_set(provenance, '{source_conversation}', to_jsonb($1::text), TRUE)
         WHERE id = $2",
    )
    .bind(compatibility_scope)
    .bind(memory_id.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl ContextReader for PostgresStore {
    async fn get_context(&self, id: &ContextId, principal: &str) -> Result<Option<ContextDefinition>, StoreError> {
        let row = query(
            "SELECT id, kind, context_key, display_name, description, owner_principal,
                    guidance, parent_id, lifecycle, frozen, created_at, updated_at
             FROM contexts AS context_row
             WHERE context_row.id = $2
               AND (
                   context_row.owner_principal = $1 OR EXISTS (
                       SELECT 1 FROM context_grants AS grant_row
                       WHERE grant_row.context_id = context_row.id
                         AND grant_row.grantee_principal IN ($1, '*')
                   )
               )",
        )
        .bind(principal)
        .bind(id.to_string())
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(parse_postgres_context_row).transpose()
    }

    async fn list_contexts(&self, principal: &str, include_archived: bool, offset: usize, limit: usize) -> Result<Vec<ContextDefinition>, StoreError> {
        let lifecycle_clause = if include_archived { "" } else { "AND context_row.lifecycle = 'active'" };
        let sql = format!(
            "SELECT id, kind, context_key, display_name, description, owner_principal,
                    guidance, parent_id, lifecycle, frozen, created_at, updated_at
             FROM contexts AS context_row
             WHERE (
                 context_row.owner_principal = $1 OR EXISTS (
                     SELECT 1 FROM context_grants AS grant_row
                     WHERE grant_row.context_id = context_row.id
                       AND grant_row.grantee_principal IN ($1, '*')
                 )
             )
             {lifecycle_clause}
             ORDER BY context_row.kind, context_row.normalized_key, context_row.id
             LIMIT $2 OFFSET $3"
        );
        let limit = i64::try_from(limit.min(MAX_CONTEXT_PAGE_SIZE)).map_err(|error| StoreError::Conflict(format!("context page limit exceeds BIGINT: {error}")))?;
        let offset = i64::try_from(offset).map_err(|error| StoreError::Conflict(format!("context page offset exceeds BIGINT: {error}")))?;
        query(AssertSqlSafe(sql.as_str()))
            .bind(principal)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await?
            .iter()
            .map(parse_postgres_context_row)
            .collect()
    }

    async fn list_context_records(&self, principal: &str, include_archived: bool, offset: usize, limit: usize) -> Result<Vec<ContextRecord>, StoreError> {
        let lifecycle_clause = if include_archived { "" } else { "AND context_row.lifecycle = 'active'" };
        let sql = format!(
            "SELECT context_row.id, context_row.kind, context_row.context_key,
                    context_row.display_name, context_row.description,
                    context_row.owner_principal, context_row.guidance,
                    context_row.parent_id, context_row.lifecycle, context_row.frozen,
                    context_row.created_at, context_row.updated_at,
                    COALESCE((
                        SELECT jsonb_agg(alias_row.alias ORDER BY alias_row.normalized_alias)
                        FROM context_aliases AS alias_row
                        WHERE alias_row.context_id = context_row.id
                    ), '[]'::jsonb) AS aliases,
                    COALESCE((
                        SELECT jsonb_agg(
                            jsonb_build_object(
                                'scheme', identity_row.scheme,
                                'namespace', NULLIF(identity_row.namespace, ''),
                                'fingerprint', identity_row.fingerprint,
                                'redacted_label', identity_row.redacted_label
                            )
                            ORDER BY identity_row.scheme, identity_row.namespace, identity_row.fingerprint
                        )
                        FROM context_identities AS identity_row
                        WHERE identity_row.context_id = context_row.id
                    ), '[]'::jsonb) AS identities,
                    COALESCE((
                        SELECT jsonb_agg(hint_row.hint ORDER BY hint_row.normalized_hint)
                        FROM context_resolver_hints AS hint_row
                        WHERE hint_row.context_id = context_row.id
                    ), '[]'::jsonb) AS hints
             FROM contexts AS context_row
             WHERE (
                 context_row.owner_principal = $1 OR EXISTS (
                     SELECT 1 FROM context_grants AS grant_row
                     WHERE grant_row.context_id = context_row.id
                       AND grant_row.grantee_principal IN ($1, '*')
                 )
             )
             {lifecycle_clause}
             ORDER BY context_row.kind, context_row.normalized_key, context_row.id
             LIMIT $2 OFFSET $3"
        );
        let limit = i64::try_from(limit.min(MAX_CONTEXT_PAGE_SIZE)).map_err(|error| StoreError::Conflict(format!("context page limit exceeds BIGINT: {error}")))?;
        let offset = i64::try_from(offset).map_err(|error| StoreError::Conflict(format!("context page offset exceeds BIGINT: {error}")))?;
        query(AssertSqlSafe(sql.as_str()))
            .bind(principal)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await?
            .iter()
            .map(|row| {
                let Json(aliases): Json<Vec<String>> = row.try_get("aliases")?;
                let Json(identities): Json<Vec<ContextIdentity>> = row.try_get("identities")?;
                let Json(hints): Json<Vec<String>> = row.try_get("hints")?;
                Ok(ContextRecord {
                    context: parse_postgres_context_row(row)?,
                    aliases,
                    identities,
                    hints,
                })
            })
            .collect()
    }

    #[expect(clippy::too_many_lines, reason = "indexed exact lookup keeps each locator's SQL and bindings explicit")]
    async fn find_context_records(&self, principal: &str, include_archived: bool, lookup: &ContextExactLookup) -> Result<Vec<ContextRecord>, StoreError> {
        let lifecycle_clause = if include_archived { "" } else { "AND context_row.lifecycle = 'active'" };
        let visibility = "(
            context_row.owner_principal = $1 OR EXISTS (
                SELECT 1 FROM context_grants AS grant_row
                WHERE grant_row.context_id = context_row.id
                  AND grant_row.grantee_principal IN ($1, '*')
            )
        )";
        let select = "SELECT context_row.id, context_row.kind, context_row.context_key,
                             context_row.display_name, context_row.description,
                             context_row.owner_principal, context_row.guidance,
                             context_row.parent_id, context_row.lifecycle, context_row.frozen,
                             context_row.created_at, context_row.updated_at
                      FROM contexts AS context_row";
        let rows = match lookup {
            ContextExactLookup::Id(id) => {
                let sql = format!(
                    "{select}
                     WHERE {visibility}
                       AND context_row.id = $2
                       {lifecycle_clause}
                     LIMIT 6"
                );
                query(AssertSqlSafe(sql.as_str())).bind(principal).bind(id.to_string()).fetch_all(self.pool()).await?
            }
            ContextExactLookup::Key { kind: Some(kind), normalized_key } => {
                let sql = format!(
                    "WITH candidate_ids(id) AS (
                         SELECT context_row.id
                         FROM contexts AS context_row
                         WHERE context_row.kind = $2
                           AND context_row.normalized_key = $3
                         UNION
                         SELECT alias_row.context_id
                         FROM context_aliases AS alias_row
                         JOIN contexts AS candidate ON candidate.id = alias_row.context_id
                         WHERE candidate.kind = $2
                           AND alias_row.normalized_alias = $3
                     )
                     {select}
                     JOIN candidate_ids AS candidate ON candidate.id = context_row.id
                     WHERE {visibility}
                       {lifecycle_clause}
                     ORDER BY context_row.normalized_key, context_row.id
                     LIMIT 6"
                );
                query(AssertSqlSafe(sql.as_str()))
                    .bind(principal)
                    .bind(kind.as_str())
                    .bind(normalized_key)
                    .fetch_all(self.pool())
                    .await?
            }
            ContextExactLookup::Key { kind: None, normalized_key } => {
                let sql = format!(
                    "WITH candidate_ids(id) AS (
                         SELECT context_row.id
                         FROM contexts AS context_row
                         WHERE context_row.normalized_key = $2
                         UNION
                         SELECT alias_row.context_id
                         FROM context_aliases AS alias_row
                         WHERE alias_row.normalized_alias = $2
                     )
                     {select}
                     JOIN candidate_ids AS candidate ON candidate.id = context_row.id
                     WHERE {visibility}
                       {lifecycle_clause}
                     ORDER BY context_row.kind, context_row.normalized_key, context_row.id
                     LIMIT 6"
                );
                query(AssertSqlSafe(sql.as_str())).bind(principal).bind(normalized_key).fetch_all(self.pool()).await?
            }
            ContextExactLookup::Identity { kind, identity } => {
                let sql = format!(
                    "WITH candidate_ids(id) AS (
                         SELECT identity_row.context_id
                         FROM context_identities AS identity_row
                         WHERE identity_row.kind = $2
                           AND identity_row.scheme = $3
                           AND identity_row.namespace = $4
                           AND identity_row.fingerprint = $5
                     )
                     {select}
                     JOIN candidate_ids AS candidate ON candidate.id = context_row.id
                     WHERE {visibility}
                       AND context_row.kind = $2
                       {lifecycle_clause}
                     ORDER BY context_row.normalized_key, context_row.id
                     LIMIT 6"
                );
                query(AssertSqlSafe(sql.as_str()))
                    .bind(principal)
                    .bind(kind.as_str())
                    .bind(&identity.scheme)
                    .bind(identity.namespace.as_deref().unwrap_or_default())
                    .bind(&identity.fingerprint)
                    .fetch_all(self.pool())
                    .await?
            }
        };
        let mut records = Vec::with_capacity(rows.len());
        for row in &rows {
            records.push(fetch_postgres_context_record(self, parse_postgres_context_row(row)?).await?);
        }
        Ok(records)
    }

    async fn expand_context_selection(&self, context_ids: &[ContextId], principal: &str, include_descendants: bool) -> Result<Vec<ContextDefinition>, StoreError> {
        for context_id in context_ids {
            let context = self
                .get_context(context_id, principal)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("context not found, archived, or not granted: {context_id}")))?;
            if context.lifecycle != ContextLifecycle::Active {
                return Err(StoreError::Conflict(format!("context {context_id} is archived and cannot be selected")));
            }
        }
        let selected_ids = context_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        let rows = query(
            "WITH RECURSIVE selected(id) AS (
                 SELECT unnest($2::text[])
             ),
             descendants(id) AS (
                 SELECT id FROM selected
                 UNION
                 SELECT child.id
                 FROM contexts AS child
                 JOIN descendants AS selected_parent ON child.parent_id = selected_parent.id
                 WHERE $3
                   AND (
                       child.owner_principal = $1 OR EXISTS (
                           SELECT 1 FROM context_grants AS grant_row
                           WHERE grant_row.context_id = child.id
                             AND grant_row.grantee_principal IN ($1, '*')
                       )
                   )
             ),
             effective(id) AS (
                 SELECT id FROM descendants
                 UNION
                 SELECT parent.id
                 FROM contexts AS parent
                 JOIN contexts AS child ON child.parent_id = parent.id
                 JOIN effective AS effective_child ON effective_child.id = child.id
                 WHERE (
                       parent.owner_principal = $1 OR EXISTS (
                           SELECT 1 FROM context_grants AS grant_row
                           WHERE grant_row.context_id = parent.id
                             AND grant_row.grantee_principal IN ($1, '*')
                       )
                   )
             )
             SELECT context_row.id, context_row.kind, context_row.context_key,
                    context_row.display_name, context_row.description, context_row.owner_principal,
                    guidance, parent_id, lifecycle, frozen, created_at, updated_at
             FROM contexts AS context_row
             JOIN effective AS selected_context ON selected_context.id = context_row.id
             ORDER BY context_row.kind, context_row.normalized_key, context_row.id",
        )
        .bind(principal)
        .bind(selected_ids)
        .bind(include_descendants)
        .fetch_all(self.pool())
        .await?;
        let definitions = rows.iter().map(parse_postgres_context_row).collect::<Result<Vec<_>, _>>()?;
        expand_context_definitions(&definitions, context_ids, include_descendants)
    }

    async fn get_memory_contexts(&self, memory_id: &MemoryId, principal: &str) -> Result<Vec<MemoryContext>, StoreError> {
        let authorization = query(
            "SELECT provenance, access_policy
             FROM memories
             WHERE id = $1
               AND (expires_at IS NULL OR expires_at > $2)",
        )
        .bind(memory_id.to_string())
        .bind(self.clock_now())
        .fetch_optional(self.pool())
        .await?;
        let Some(authorization) = authorization else {
            return Err(StoreError::NotFound(format!("memory not found: {memory_id}")));
        };
        let Json(provenance): Json<Provenance> = authorization.try_get("provenance")?;
        let Json(access_policy): Json<AccessPolicy> = authorization.try_get("access_policy")?;
        if !memory_read_allowed(&provenance, &access_policy, principal) {
            return Err(StoreError::NotFound(format!("memory not found: {memory_id}")));
        }
        let rows = query(
            "SELECT context_row.id, context_row.kind, context_row.context_key,
                    context_row.display_name, context_row.description,
                    context_row.owner_principal, context_row.guidance,
                    context_row.parent_id, context_row.lifecycle, context_row.frozen,
                    context_row.created_at, context_row.updated_at,
                    membership.ordinal
             FROM memory_contexts AS membership
             JOIN contexts AS context_row ON context_row.id = membership.context_id
             WHERE membership.memory_id = $2
               AND (
                   context_row.owner_principal = $1 OR EXISTS (
                       SELECT 1 FROM context_grants AS grant_row
                       WHERE grant_row.context_id = context_row.id
                         AND grant_row.grantee_principal IN ($1, '*')
                   )
               )
             ORDER BY membership.ordinal",
        )
        .bind(principal)
        .bind(memory_id.to_string())
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let ordinal: i64 = row.try_get("ordinal")?;
                Ok(MemoryContext {
                    memory_id: *memory_id,
                    context: parse_postgres_context_row(row)?,
                    ordinal: u32::try_from(ordinal).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                })
            })
            .collect()
    }

    async fn get_memory_contexts_batch(&self, memory_ids: &[MemoryId], principal: &str) -> Result<HashMap<MemoryId, Vec<MemoryContext>>, StoreError> {
        if memory_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids = memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        let rows = query(
            "SELECT context_row.id, context_row.kind, context_row.context_key,
                    context_row.display_name, context_row.description,
                    context_row.owner_principal, context_row.guidance,
                    context_row.parent_id, context_row.lifecycle, context_row.frozen,
                    context_row.created_at, context_row.updated_at,
                    membership.ordinal, membership.memory_id,
                    memory_row.provenance, memory_row.access_policy
             FROM memory_contexts AS membership
             JOIN memories AS memory_row ON memory_row.id = membership.memory_id
             JOIN contexts AS context_row ON context_row.id = membership.context_id
             WHERE membership.memory_id = ANY($2)
               AND (memory_row.expires_at IS NULL OR memory_row.expires_at > $3)
               AND (
                   context_row.owner_principal = $1 OR EXISTS (
                       SELECT 1 FROM context_grants AS grant_row
                       WHERE grant_row.context_id = context_row.id
                         AND grant_row.grantee_principal IN ($1, '*')
                   )
               )
             ORDER BY membership.memory_id, membership.ordinal",
        )
        .bind(principal)
        .bind(ids)
        .bind(self.clock_now())
        .fetch_all(self.pool())
        .await?;
        let mut memberships = HashMap::<MemoryId, Vec<MemoryContext>>::new();
        for row in &rows {
            let Json(provenance): Json<Provenance> = row.try_get("provenance")?;
            let Json(access_policy): Json<AccessPolicy> = row.try_get("access_policy")?;
            if !memory_read_allowed(&provenance, &access_policy, principal) {
                continue;
            }
            let memory_id = MemoryId::from_str(&row.try_get::<String, _>("memory_id")?).map_err(|error| StoreError::Serialization(Box::new(error)))?;
            let ordinal: i64 = row.try_get("ordinal")?;
            memberships.entry(memory_id).or_default().push(MemoryContext {
                memory_id,
                context: parse_postgres_context_row(row)?,
                ordinal: u32::try_from(ordinal).map_err(|error| StoreError::Serialization(Box::new(error)))?,
            });
        }
        Ok(memberships)
    }

    async fn get_memory_context_presence_batch(&self, memory_ids: &[MemoryId], principal: &str) -> Result<MemoryContextPresence, StoreError> {
        if memory_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let ids = memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        let rows = query(
            "SELECT DISTINCT memory_row.id, memory_row.provenance, memory_row.access_policy
             FROM memories AS memory_row
             JOIN memory_contexts AS membership ON membership.memory_id = memory_row.id
             WHERE memory_row.id = ANY($1)
               AND (memory_row.expires_at IS NULL OR memory_row.expires_at > $2)",
        )
        .bind(ids)
        .bind(self.clock_now())
        .fetch_all(self.pool())
        .await?;
        let mut present = HashSet::new();
        for row in &rows {
            let Json(provenance): Json<Provenance> = row.try_get("provenance")?;
            let Json(access_policy): Json<AccessPolicy> = row.try_get("access_policy")?;
            if memory_read_allowed(&provenance, &access_policy, principal) {
                let memory_id = MemoryId::from_str(&row.try_get::<String, _>("id")?).map_err(|error| StoreError::Serialization(Box::new(error)))?;
                let _inserted = present.insert(memory_id);
            }
        }
        Ok(present)
    }

    async fn count_memory_contexts_for_write(&self, memory_id: &MemoryId, principal: &str) -> Result<Option<usize>, StoreError> {
        let authorization = query("SELECT provenance, access_policy FROM memories WHERE id = $1")
            .bind(memory_id.to_string())
            .fetch_optional(self.pool())
            .await?;
        let Some(authorization) = authorization else {
            return Ok(None);
        };
        let Json(provenance): Json<Provenance> = authorization.try_get("provenance")?;
        let Json(access_policy): Json<AccessPolicy> = authorization.try_get("access_policy")?;
        if !write_access_allowed(&provenance, &access_policy, principal) {
            return Ok(None);
        }
        let count: i64 = query_scalar("SELECT COUNT(*) FROM memory_contexts WHERE memory_id = $1")
            .bind(memory_id.to_string())
            .fetch_one(self.pool())
            .await?;
        let count = usize::try_from(count).map_err(|error| StoreError::Serialization(Box::new(error)))?;
        Ok(Some(count))
    }

    async fn query_context_audit(&self, context_id: &ContextId, principal: &str, limit: usize) -> Result<Vec<ContextAuditEvent>, StoreError> {
        let Some(context) = self.get_context(context_id, principal).await? else {
            return Err(StoreError::NotFound(format!("context not found: {context_id}")));
        };
        if context.owner_principal != principal {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit.min(MAX_CONTEXT_AUDIT_PAGE_SIZE)).map_err(|error| StoreError::Conflict(format!("context audit limit exceeds BIGINT: {error}")))?;
        let rows = query(
            "SELECT id, actor_principal, action, context_id, memory_id, timestamp, details
             FROM context_audit_events
             WHERE context_id = $1
             ORDER BY id DESC
             LIMIT $2",
        )
        .bind(context_id.to_string())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let stored_context_id: Option<String> = row.try_get("context_id")?;
                let stored_memory_id: Option<String> = row.try_get("memory_id")?;
                let details: Option<Json<serde_json::Value>> = row.try_get("details")?;
                Ok(ContextAuditEvent {
                    id: row.try_get("id")?,
                    actor_principal: row.try_get("actor_principal")?,
                    action: row.try_get("action")?,
                    context_id: stored_context_id
                        .map(|id| ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
                        .transpose()?,
                    memory_id: stored_memory_id
                        .map(|id| MemoryId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
                        .transpose()?,
                    timestamp: row.try_get("timestamp")?,
                    details: details.map(|Json(value)| value),
                })
            })
            .collect()
    }

    async fn list_context_kinds(&self) -> Result<Vec<ContextKindDefinition>, StoreError> {
        let rows = query(
            "SELECT kind, display_name, builtin, enabled, created_at, updated_at
             FROM context_kinds
             ORDER BY builtin DESC, kind",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ContextKindDefinition {
                    kind: ContextKind::new(row.try_get::<String, _>("kind")?).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    display_name: row.try_get("display_name")?,
                    builtin: row.try_get("builtin")?,
                    enabled: row.try_get("enabled")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }

    async fn list_context_kind_policies(&self, principal: &str) -> Result<Vec<ContextKindPolicyRecord>, StoreError> {
        let rows = query(
            "SELECT layer, principal, kind, policy_json, updated_at
             FROM context_kind_policies
             WHERE layer = 'operator' OR (layer = 'principal' AND principal = $1)
             ORDER BY layer, kind",
        )
        .bind(principal)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let Json(policy): Json<ContextKindPolicy> = row.try_get("policy_json")?;
                Ok(ContextKindPolicyRecord {
                    layer: ContextPolicyLayer::from_str(&row.try_get::<String, _>("layer")?).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    principal: row.try_get("principal")?,
                    kind: ContextKind::new(row.try_get::<String, _>("kind")?).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    policy,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }

    async fn list_context_anchor_policies(&self, principal: &str) -> Result<Vec<ContextAnchorPolicyRecord>, StoreError> {
        let rows = query(
            "SELECT override_row.anchor_context_id, override_row.principal,
                    override_row.policy_json, override_row.updated_at
             FROM context_anchor_overrides AS override_row
             JOIN contexts AS context_row ON context_row.id = override_row.anchor_context_id
             WHERE override_row.principal = $1
               AND context_row.lifecycle = 'active'
               AND (
                   context_row.owner_principal = $1 OR EXISTS (
                       SELECT 1 FROM context_grants AS grant_row
                       WHERE grant_row.context_id = context_row.id
                         AND grant_row.grantee_principal IN ($1, $2)
                   )
               )
             ORDER BY override_row.anchor_context_id",
        )
        .bind(principal)
        .bind(LEGACY_ALL_PRINCIPALS_GRANT)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let anchor_id: String = row.try_get("anchor_context_id")?;
                let Json(policy): Json<ContextAnchorPolicy> = row.try_get("policy_json")?;
                Ok(ContextAnchorPolicyRecord {
                    anchor_context_id: ContextId::from_str(&anchor_id).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    principal: row.try_get("principal")?,
                    policy,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }

    async fn list_context_grants(&self, context_id: &ContextId, principal: &str) -> Result<Vec<ContextGrant>, StoreError> {
        let Some(context) = self.get_context(context_id, principal).await? else {
            return Err(StoreError::NotFound(format!("context not found: {context_id}")));
        };
        if context.owner_principal != principal {
            return Ok(Vec::new());
        }
        let rows = query(
            "SELECT grantee_principal, granted_by, created_at
             FROM context_grants
             WHERE context_id = $1
             ORDER BY grantee_principal",
        )
        .bind(context_id.to_string())
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ContextGrant {
                    context_id: *context_id,
                    grantee_principal: row.try_get("grantee_principal")?,
                    granted_by: row.try_get("granted_by")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }
}

impl ContextWriter for PostgresStore {
    #[expect(
        clippy::too_many_lines,
        reason = "PostgreSQL context creation keeps definition, aliases, identities, hints, and audit in one transaction"
    )]
    async fn create_context(&self, draft: &ContextCreateDraft, audit: &ContextAuditDraft) -> Result<ContextDefinition, StoreError> {
        validate_create_draft(draft, audit)?;
        let mut tx = self.pool().begin().await?;
        let _locked = query("LOCK TABLE contexts, context_aliases, context_identities, context_grants IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await?;
        let kind_enabled: Option<bool> = query_scalar("SELECT enabled FROM context_kinds WHERE kind = $1")
            .bind(draft.kind.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        let Some(kind_enabled) = kind_enabled else {
            return Err(StoreError::Conflict(format!("unknown context kind {:?}", draft.kind.as_str())));
        };
        if !kind_enabled {
            return Err(StoreError::Conflict(format!("context kind {:?} is disabled", draft.kind.as_str())));
        }
        if let Some(parent_id) = draft.parent_id
            && !postgres_context_use_allowed(&mut tx, &parent_id, &draft.owner_principal, true).await?
        {
            return Err(StoreError::Conflict(format!("parent context {parent_id} is unavailable, archived, or not granted")));
        }
        let exact_key_or_alias: bool = query_scalar(
            "SELECT EXISTS(
                 SELECT 1
                 FROM contexts AS context_row
                 WHERE context_row.kind = $2
                   AND (
                       context_row.owner_principal = $1 OR EXISTS (
                           SELECT 1
                           FROM context_grants AS grant_row
                           WHERE grant_row.context_id = context_row.id
                             AND grant_row.grantee_principal IN ($1, '*')
                       )
                   )
                   AND (
                       context_row.normalized_key = $3 OR EXISTS (
                           SELECT 1
                           FROM context_aliases AS alias_row
                           WHERE alias_row.context_id = context_row.id
                             AND alias_row.normalized_alias = $3
                       )
                   )
             )",
        )
        .bind(&draft.owner_principal)
        .bind(draft.kind.as_str())
        .bind(&draft.normalized_key)
        .fetch_one(&mut *tx)
        .await?;
        let mut exact_identity = false;
        for identity in &draft.identities {
            exact_identity = query_scalar(
                "SELECT EXISTS(
                     SELECT 1
                     FROM context_identities AS identity_row
                     JOIN contexts AS context_row ON context_row.id = identity_row.context_id
                     WHERE context_row.kind = $2
                       AND (
                           context_row.owner_principal = $1 OR EXISTS (
                               SELECT 1
                               FROM context_grants AS grant_row
                               WHERE grant_row.context_id = context_row.id
                                 AND grant_row.grantee_principal IN ($1, '*')
                           )
                       )
                       AND identity_row.scheme = $3
                       AND identity_row.namespace = $4
                       AND identity_row.fingerprint = $5
                 )",
            )
            .bind(&draft.owner_principal)
            .bind(draft.kind.as_str())
            .bind(&identity.scheme)
            .bind(identity.namespace.as_deref().unwrap_or_default())
            .bind(&identity.fingerprint)
            .fetch_one(&mut *tx)
            .await?;
            if exact_identity {
                break;
            }
        }
        if exact_key_or_alias || exact_identity {
            return Err(StoreError::Conflict("exact visible context key, alias, or identity already exists".into()));
        }
        let existing_rows = query(
            "SELECT context_row.id, context_row.context_key, context_row.display_name,
                    COALESCE(
                        jsonb_agg(alias_row.alias ORDER BY alias_row.normalized_alias)
                            FILTER (WHERE alias_row.alias IS NOT NULL),
                        '[]'::jsonb
                    ) AS aliases
             FROM contexts AS context_row
             LEFT JOIN context_aliases AS alias_row ON alias_row.context_id = context_row.id
             WHERE context_row.kind = $2
               AND context_row.lifecycle = 'active'
               AND (
                   context_row.owner_principal = $1 OR EXISTS (
                       SELECT 1 FROM context_grants AS grant_row
                       WHERE grant_row.context_id = context_row.id
                         AND grant_row.grantee_principal IN ($1, '*')
                   )
               )
             GROUP BY context_row.id, context_row.context_key, context_row.display_name
             ORDER BY context_row.normalized_key, context_row.id",
        )
        .bind(&draft.owner_principal)
        .bind(draft.kind.as_str())
        .fetch_all(&mut *tx)
        .await?;
        let existing = existing_rows
            .iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                let Json(aliases): Json<Vec<String>> = row.try_get("aliases")?;
                Ok((
                    ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                    row.try_get("context_key")?,
                    row.try_get("display_name")?,
                    aliases,
                ))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        validate_fuzzy_confirmation(draft, existing)?;
        let duplicate: bool = query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM contexts
                WHERE id = $1 OR (
                    owner_principal = $2 AND kind = $3 AND normalized_key = $4
                )
             )",
        )
        .bind(draft.id.to_string())
        .bind(&draft.owner_principal)
        .bind(draft.kind.as_str())
        .bind(&draft.normalized_key)
        .fetch_one(&mut *tx)
        .await?;
        if duplicate {
            return Err(StoreError::Conflict("context ID or owner/kind/key already exists".into()));
        }
        let now = self.clock_now();
        let _inserted = query(
            "INSERT INTO contexts (
                id, kind, context_key, normalized_key, display_name, description,
                owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'active', $10, $11, $11)",
        )
        .bind(draft.id.to_string())
        .bind(draft.kind.as_str())
        .bind(&draft.key)
        .bind(&draft.normalized_key)
        .bind(&draft.display_name)
        .bind(&draft.description)
        .bind(&draft.owner_principal)
        .bind(&draft.guidance)
        .bind(draft.parent_id.map(|id| id.to_string()))
        .bind(draft.frozen)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        for (alias, normalized) in &draft.aliases {
            let _inserted = query(
                "INSERT INTO context_aliases (context_id, alias, normalized_alias, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(draft.id.to_string())
            .bind(alias)
            .bind(normalized)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        for identity in &draft.identities {
            let _inserted = query(
                "INSERT INTO context_identities (
                    context_id, owner_principal, kind, scheme, namespace,
                    fingerprint, redacted_label, created_at
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(draft.id.to_string())
            .bind(&draft.owner_principal)
            .bind(draft.kind.as_str())
            .bind(&identity.scheme)
            .bind(identity.namespace.as_deref().unwrap_or_default())
            .bind(&identity.fingerprint)
            .bind(&identity.redacted_label)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        let mut hints = HashSet::new();
        for hint in &draft.resolver_hints {
            let normalized = normalize_context_key(hint);
            if normalized.is_empty() || !hints.insert(normalized.clone()) {
                continue;
            }
            let _inserted = query(
                "INSERT INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(draft.id.to_string())
            .bind(hint)
            .bind(normalized)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        insert_postgres_context_audit(&mut tx, audit, Some(&draft.id), None, now).await?;
        tx.commit().await?;
        Ok(ContextDefinition {
            id: draft.id,
            kind: draft.kind.clone(),
            key: draft.key.clone(),
            display_name: draft.display_name.clone(),
            description: draft.description.clone(),
            owner_principal: draft.owner_principal.clone(),
            guidance: draft.guidance.clone(),
            parent_id: draft.parent_id,
            lifecycle: ContextLifecycle::Active,
            frozen: draft.frozen,
            created_at: now,
            updated_at: now,
        })
    }

    async fn set_context_parent(&self, context_id: &ContextId, parent_id: Option<&ContextId>, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        if parent_id == Some(context_id) {
            return Err(StoreError::Conflict("a context cannot be its own parent".into()));
        }
        let mut tx = self.pool().begin().await?;
        let _locked = query("LOCK TABLE contexts IN SHARE ROW EXCLUSIVE MODE").execute(&mut *tx).await?;
        require_postgres_mutable_owned_context(&mut tx, context_id, principal).await?;
        if let Some(parent_id) = parent_id {
            if !postgres_context_use_allowed(&mut tx, parent_id, principal, true).await? {
                return Err(StoreError::Conflict(format!("parent context {parent_id} is unavailable, archived, or not granted")));
            }
            let cycle: bool = query_scalar(
                "WITH RECURSIVE ancestors(id, parent_id) AS (
                    SELECT id, parent_id FROM contexts WHERE id = $1
                    UNION
                    SELECT parent.id, parent.parent_id
                    FROM contexts AS parent
                    JOIN ancestors ON parent.id = ancestors.parent_id
                 )
                 SELECT EXISTS(SELECT 1 FROM ancestors WHERE id = $2)",
            )
            .bind(parent_id.to_string())
            .bind(context_id.to_string())
            .fetch_one(&mut *tx)
            .await?;
            if cycle {
                return Err(StoreError::Conflict(format!(
                    "setting context {context_id} parent to {parent_id} would create a hierarchy cycle"
                )));
            }
        }
        let now = self.clock_now();
        let changed = query("UPDATE contexts SET parent_id = $1, updated_at = $2 WHERE id = $3")
            .bind(parent_id.map(ToString::to_string))
            .bind(now)
            .bind(context_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if changed != 1 {
            return Err(StoreError::Conflict(format!("context {context_id} changed during parent update")));
        }
        insert_postgres_context_audit(&mut tx, audit, Some(context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn set_context_lifecycle(&self, context_id: &ContextId, lifecycle: ContextLifecycle, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        let mut tx = self.pool().begin().await?;
        require_postgres_mutable_owned_context(&mut tx, context_id, principal).await?;
        let now = self.clock_now();
        let changed = query("UPDATE contexts SET lifecycle = $1, updated_at = $2 WHERE id = $3")
            .bind(lifecycle.to_string())
            .bind(now)
            .bind(context_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if changed != 1 {
            return Err(StoreError::Conflict(format!("context {context_id} changed during lifecycle update")));
        }
        insert_postgres_context_audit(&mut tx, audit, Some(context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn grant_context_use(&self, context_id: &ContextId, grantee_principal: &str, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        let grantee_principal = normalize_explicit_grantee(grantee_principal)?;
        let mut tx = self.pool().begin().await?;
        require_postgres_mutable_owned_context(&mut tx, context_id, principal).await?;
        let now = self.clock_now();
        let _changed = query(
            "INSERT INTO context_grants (context_id, grantee_principal, granted_by, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT(context_id, grantee_principal) DO UPDATE SET
                granted_by = excluded.granted_by,
                created_at = excluded.created_at",
        )
        .bind(context_id.to_string())
        .bind(grantee_principal)
        .bind(principal)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        insert_postgres_context_audit(&mut tx, audit, Some(context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn revoke_context_use(&self, context_id: &ContextId, grantee_principal: &str, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        let grantee_principal = normalize_explicit_grantee(grantee_principal)?;
        let mut tx = self.pool().begin().await?;
        require_postgres_mutable_owned_context(&mut tx, context_id, principal).await?;
        let now = self.clock_now();
        let _removed = query("DELETE FROM context_grants WHERE context_id = $1 AND grantee_principal = $2")
            .bind(context_id.to_string())
            .bind(grantee_principal)
            .execute(&mut *tx)
            .await?;
        insert_postgres_context_audit(&mut tx, audit, Some(context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn replace_context_grants(&self, context_id: &ContextId, grantee_principals: &[String], principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        let grantees = normalize_explicit_grantees(grantee_principals)?;
        let mut tx = self.pool().begin().await?;
        require_postgres_mutable_owned_context(&mut tx, context_id, principal).await?;
        let now = self.clock_now();
        let _removed = query("DELETE FROM context_grants WHERE context_id = $1")
            .bind(context_id.to_string())
            .execute(&mut *tx)
            .await?;
        for grantee in grantees {
            let _inserted = query(
                "INSERT INTO context_grants (context_id, grantee_principal, granted_by, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(context_id.to_string())
            .bind(grantee)
            .bind(principal)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        insert_postgres_context_audit(&mut tx, audit, Some(context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "definition replacement atomically validates and replaces aliases, identities, hints, and audit"
    )]
    async fn update_context_definition(&self, context_id: &ContextId, patch: &ContextDefinitionPatch, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        validate_definition_patch_surfaces(patch)?;
        let display_name = patch.display_name.trim();
        let mut aliases = Vec::new();
        let mut normalized_aliases = HashSet::new();
        for alias in &patch.aliases {
            let alias = alias.trim();
            let normalized = normalize_context_key(alias);
            if alias.is_empty() || normalized.is_empty() || !normalized_aliases.insert(normalized.clone()) {
                return Err(StoreError::Conflict("context aliases must be non-empty and unique after normalization".into()));
            }
            aliases.push((alias.to_owned(), normalized));
        }
        let mut identities = HashSet::new();
        for identity in &patch.identities {
            validate_identity(identity)?;
            if !identities.insert((identity.scheme.clone(), identity.namespace.clone(), identity.fingerprint.clone())) {
                return Err(StoreError::Conflict("context identities must be unique".into()));
            }
        }
        let mut hints = Vec::new();
        let mut normalized_hints = HashSet::new();
        for hint in &patch.resolver_hints {
            let hint = hint.trim();
            let normalized = normalize_context_key(hint);
            if hint.is_empty() || normalized.is_empty() || !normalized_hints.insert(normalized.clone()) {
                return Err(StoreError::Conflict("context resolver hints must be non-empty and unique after normalization".into()));
            }
            hints.push((hint.to_owned(), normalized));
        }

        let mut tx = self.pool().begin().await?;
        require_postgres_mutable_owned_context(&mut tx, context_id, principal).await?;
        let owner_kind = query("SELECT owner_principal, kind FROM contexts WHERE id = $1")
            .bind(context_id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        let owner: String = owner_kind.try_get("owner_principal")?;
        let kind: String = owner_kind.try_get("kind")?;
        let now = self.clock_now();
        let _updated = query(
            "UPDATE contexts
             SET display_name = $1, description = $2, guidance = $3, updated_at = $4
             WHERE id = $5",
        )
        .bind(display_name)
        .bind(patch.description.as_deref().map(str::trim).filter(|value| !value.is_empty()))
        .bind(patch.guidance.as_deref().map(str::trim).filter(|value| !value.is_empty()))
        .bind(now)
        .bind(context_id.to_string())
        .execute(&mut *tx)
        .await?;
        let _aliases_removed = query("DELETE FROM context_aliases WHERE context_id = $1")
            .bind(context_id.to_string())
            .execute(&mut *tx)
            .await?;
        for (alias, normalized) in aliases {
            let _inserted = query(
                "INSERT INTO context_aliases (context_id, alias, normalized_alias, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(context_id.to_string())
            .bind(alias)
            .bind(normalized)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        let _identities_removed = query("DELETE FROM context_identities WHERE context_id = $1")
            .bind(context_id.to_string())
            .execute(&mut *tx)
            .await?;
        for identity in &patch.identities {
            let _inserted = query(
                "INSERT INTO context_identities (
                    context_id, owner_principal, kind, scheme, namespace,
                    fingerprint, redacted_label, created_at
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(context_id.to_string())
            .bind(&owner)
            .bind(&kind)
            .bind(&identity.scheme)
            .bind(identity.namespace.as_deref().unwrap_or_default())
            .bind(&identity.fingerprint)
            .bind(&identity.redacted_label)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        let _hints_removed = query("DELETE FROM context_resolver_hints WHERE context_id = $1")
            .bind(context_id.to_string())
            .execute(&mut *tx)
            .await?;
        for (hint, normalized) in hints {
            let _inserted = query(
                "INSERT INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(context_id.to_string())
            .bind(hint)
            .bind(normalized)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        insert_postgres_context_audit(&mut tx, audit, Some(context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn upsert_context_kind(&self, draft: &ContextKindDraft, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        if principal != OPERATOR_PRINCIPAL {
            return Err(StoreError::Conflict(format!("context kind mutation requires principal {OPERATOR_PRINCIPAL:?}")));
        }
        let display_name = draft.display_name.trim();
        if display_name.is_empty() {
            return Err(StoreError::Conflict("context kind display name cannot be blank".into()));
        }
        let mut tx = self.pool().begin().await?;
        let now = self.clock_now();
        let _changed = query(
            "INSERT INTO context_kinds (
                kind, display_name, builtin, enabled, created_at, updated_at
             ) VALUES ($1, $2, FALSE, $3, $4, $4)
             ON CONFLICT(kind) DO UPDATE SET
                display_name = excluded.display_name,
                enabled = excluded.enabled,
                updated_at = excluded.updated_at",
        )
        .bind(draft.kind.as_str())
        .bind(display_name)
        .bind(draft.enabled)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        insert_postgres_context_audit(&mut tx, audit, None, None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn upsert_context_kind_policy(&self, draft: &ContextKindPolicyDraft, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        draft.policy.validate().map_err(StoreError::Conflict)?;
        let stored_principal = match draft.layer {
            ContextPolicyLayer::Operator => {
                if !draft.principal.is_empty() {
                    return Err(StoreError::Conflict("operator policy principal must be empty".into()));
                }
                if principal != OPERATOR_PRINCIPAL {
                    return Err(StoreError::Conflict(format!("operator policy mutation requires principal {OPERATOR_PRINCIPAL:?}")));
                }
                ""
            }
            ContextPolicyLayer::Principal => {
                if draft.principal != principal {
                    return Err(StoreError::Conflict("a principal policy may only customize its own principal".into()));
                }
                principal
            }
        };
        let mut tx = self.pool().begin().await?;
        validate_policy_default_postgres(&mut tx, &draft.kind, &draft.policy, stored_principal).await?;
        let now = self.clock_now();
        let _changed = query(
            "INSERT INTO context_kind_policies (
                layer, principal, kind, policy_json, updated_at
             ) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT(layer, principal, kind) DO UPDATE SET
                policy_json = excluded.policy_json,
                updated_at = excluded.updated_at",
        )
        .bind(draft.layer.to_string())
        .bind(stored_principal)
        .bind(draft.kind.as_str())
        .bind(Json(draft.policy.clone()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        insert_postgres_context_audit(&mut tx, audit, None, None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn upsert_context_anchor_policy(&self, draft: &ContextAnchorPolicyDraft, principal: &str, audit: &ContextAuditDraft) -> Result<(), StoreError> {
        validate_audit_actor(audit, principal)?;
        if draft.principal != principal {
            return Err(StoreError::Conflict("an anchor policy may only customize its own principal".into()));
        }
        draft.policy.validate().map_err(StoreError::Conflict)?;
        let mut tx = self.pool().begin().await?;
        if !postgres_context_use_allowed(&mut tx, &draft.anchor_context_id, principal, true).await? {
            return Err(StoreError::Conflict("anchor context is unavailable, archived, or not granted".into()));
        }
        for (kind, policy) in &draft.policy.kinds {
            let kind = ContextKind::new(kind.clone()).map_err(|error| StoreError::Serialization(Box::new(error)))?;
            validate_policy_default_postgres(&mut tx, &kind, policy, principal).await?;
        }
        let now = self.clock_now();
        let _changed = query(
            "INSERT INTO context_anchor_overrides (
                anchor_context_id, principal, policy_json, updated_at
             ) VALUES ($1, $2, $3, $4)
             ON CONFLICT(anchor_context_id, principal) DO UPDATE SET
                policy_json = excluded.policy_json,
                updated_at = excluded.updated_at",
        )
        .bind(draft.anchor_context_id.to_string())
        .bind(principal)
        .bind(Json(draft.policy.clone()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        insert_postgres_context_audit(&mut tx, audit, Some(&draft.anchor_context_id), None, now).await?;
        tx.commit().await?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "PostgreSQL compensation checks every relational and JSON policy reference before its atomic delete"
    )]
    async fn rollback_unreferenced_legacy_context(&self, context_id: &ContextId, principal: &str) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let locked: Option<String> = query_scalar("SELECT id FROM contexts WHERE id = $1 FOR UPDATE")
            .bind(context_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
        if locked.is_none() {
            tx.commit().await?;
            return Ok(false);
        }
        let removed = query(
            "DELETE FROM contexts
             WHERE id = $1
               AND owner_principal = $2
               AND kind = 'custom'
               AND frozen = FALSE
               AND NOT EXISTS (SELECT 1 FROM memory_contexts WHERE context_id = $1)
               AND NOT EXISTS (SELECT 1 FROM contexts WHERE parent_id = $1)
               AND NOT EXISTS (SELECT 1 FROM context_grants WHERE context_id = $1)
               AND NOT EXISTS (
                   SELECT 1 FROM context_relations
                   WHERE from_context_id = $1 OR to_context_id = $1
               )
               AND NOT EXISTS (SELECT 1 FROM context_aliases WHERE context_id = $1)
               AND NOT EXISTS (SELECT 1 FROM context_identities WHERE context_id = $1)
               AND NOT EXISTS (SELECT 1 FROM context_resolver_hints WHERE context_id = $1)
               AND NOT EXISTS (SELECT 1 FROM context_anchor_overrides WHERE anchor_context_id = $1)
               AND NOT EXISTS (
                   SELECT 1
                   FROM context_kind_policies AS policy
                   CROSS JOIN LATERAL jsonb_path_query(
                       policy.policy_json,
                       'strict $.**.default_context_id'
                   ) AS default_id
                   WHERE default_id = to_jsonb($1::text)
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM context_anchor_overrides AS policy
                   CROSS JOIN LATERAL jsonb_path_query(
                       policy.policy_json,
                       'strict $.**.default_context_id'
                   ) AS default_id
                   WHERE default_id = to_jsonb($1::text)
               )
               AND (
                   SELECT COUNT(*) FROM context_audit_events
                   WHERE context_id = $1
               ) = 1
               AND EXISTS (
                   SELECT 1 FROM context_audit_events
                   WHERE context_id = $1
                     AND actor_principal = $2
                     AND action = 'legacy_scope_context_created'
                     AND memory_id IS NULL
               )",
        )
        .bind(context_id.to_string())
        .bind(principal)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if removed == 1 {
            let _removed_audit = query(
                "DELETE FROM context_audit_events
                 WHERE context_id = $1
                   AND actor_principal = $2
                   AND action = 'legacy_scope_context_created'
                   AND memory_id IS NULL",
            )
            .bind(context_id.to_string())
            .bind(principal)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(removed == 1)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "membership replacement keeps authorization, locking, cache synchronization, and audit in one transaction"
    )]
    async fn replace_memory_contexts(&self, memory_id: &MemoryId, context_ids: &[ContextId], principal: &str, audit: &ContextAuditDraft) -> Result<WriteOutcome, StoreError> {
        validate_audit_actor(audit, principal)?;
        let unique = context_ids.iter().copied().collect::<HashSet<_>>();
        if unique.len() != context_ids.len() {
            return Err(StoreError::Conflict("memory context memberships must be unique".into()));
        }
        let mut tx = self.pool().begin().await?;
        let authorization = query("SELECT provenance, access_policy FROM memories WHERE id = $1 FOR UPDATE")
            .bind(memory_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
        let Some(authorization) = authorization else {
            return Ok(WriteOutcome::NotFound);
        };
        let Json(provenance): Json<Provenance> = authorization.try_get("provenance")?;
        let Json(access_policy): Json<AccessPolicy> = authorization.try_get("access_policy")?;
        if !write_access_allowed(&provenance, &access_policy, principal) {
            return Ok(WriteOutcome::Denied);
        }
        let preserved_context_ids = query_scalar::<Postgres, String>("SELECT context_id FROM memory_contexts WHERE memory_id = $1")
            .bind(memory_id.to_string())
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|id| ContextId::from_str(&id).map_err(|error| StoreError::Serialization(Box::new(error))))
            .collect::<Result<HashSet<_>, _>>()?;
        lock_postgres_contexts_for_membership(&mut tx, context_ids).await?;
        for context_id in context_ids {
            let require_active = !preserved_context_ids.contains(context_id);
            if !postgres_context_use_allowed(&mut tx, context_id, principal, require_active).await? {
                return Err(StoreError::Conflict(format!(
                    "context {context_id} is unavailable, archived, or not granted to principal {principal:?}"
                )));
            }
        }
        let now = self.clock_now();
        let _removed = query("DELETE FROM memory_contexts WHERE memory_id = $1")
            .bind(memory_id.to_string())
            .execute(&mut *tx)
            .await?;
        for (ordinal, context_id) in context_ids.iter().enumerate() {
            let ordinal = i64::try_from(ordinal).map_err(|error| StoreError::Conflict(format!("context membership ordinal exceeds BIGINT: {error}")))?;
            let _inserted = query(
                "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(memory_id.to_string())
            .bind(context_id.to_string())
            .bind(ordinal)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        let primary_key: String = if let Some(primary) = context_ids.first() {
            query_scalar::<Postgres, String>("SELECT context_key FROM contexts WHERE id = $1")
                .bind(primary.to_string())
                .fetch_one(&mut *tx)
                .await?
        } else {
            UNRESOLVED_SCOPE.into()
        };
        let _updated_metadata = query("UPDATE memory_metadata SET scope_key = $1, updated_at = $2 WHERE memory_id = $3")
            .bind(&primary_key)
            .bind(now)
            .bind(memory_id.to_string())
            .execute(&mut *tx)
            .await?;
        let _updated_memory = query(
            "UPDATE memories
             SET provenance = jsonb_set(provenance, '{source_conversation}', to_jsonb($1::text), TRUE),
                 record_revision = record_revision + 1
             WHERE id = $2",
        )
        .bind(&primary_key)
        .bind(memory_id.to_string())
        .execute(&mut *tx)
        .await?;
        insert_postgres_context_audit(&mut tx, audit, None, Some(memory_id), now).await?;
        tx.commit().await?;
        Ok(WriteOutcome::Applied)
    }
}
