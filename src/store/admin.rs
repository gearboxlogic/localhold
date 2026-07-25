//! Administrative operations — eviction, scope reassignment, embedding management, and statistics.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::{
    ExpiredCleanupScope, MemoryAuthorizationEnvelope, MemoryAuthorizationRef, ReassignScopeOutcome, SqliteStore,
    crud::{SQLITE_MAX_CHUNK, fetch_memory_by_id, get_metadata_conn, insert_audit_draft, insert_authorization_tombstone, upsert_metadata_conn},
    query::{DEFAULT_LIST_LIMIT, OVERFETCH_FACTOR, ScanConfig, count_with_access_filter, normalize_filter},
    sqlite_write_tx,
    vector::{VectorIndex as _, validate_embedding_vector},
};
use crate::{
    context::{
        ContextId, ContextKind, LEGACY_ALL_PRINCIPALS_GRANT, LEGACY_SYSTEM_PRINCIPAL, OPERATOR_PRINCIPAL, UNRESOLVED_CONTEXT_KEY as UNRESOLVED_SCOPE,
        validate_implicit_legacy_context_key, validate_legacy_scope_definition,
    },
    error::StoreError,
    types::{
        AccessPolicy, AuditDraft, LARGE_CONTENT_WARNING_THRESHOLD_BYTES, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryStats, MetadataMigrationOutcome,
        MetadataMigrationReport, Provenance, QueryContext, ScopeDefinition, normalize_context_key,
    },
};

fn sqlite_count(row: &rusqlite::Row<'_>) -> rusqlite::Result<u64> {
    let count: i64 = row.get(0)?;
    u64::try_from(count).map_err(|_err| rusqlite::Error::IntegralValueOutOfRange(0, count))
}

#[expect(clippy::multiple_inherent_impl, reason = "SqliteStore methods are split across submodules by concern")]
impl SqliteStore {
    pub(crate) async fn evict_expired_impl(&self, principal: &str, audit: &AuditDraft) -> Result<u64, StoreError> {
        self.evict_expired_with_scope_impl(ExpiredCleanupScope::Authorized { actor: principal.to_owned() }, audit)
            .await
    }

    pub(crate) async fn evict_expired_all_impl(&self, principal: &str, audit: &AuditDraft) -> Result<u64, StoreError> {
        self.evict_expired_with_scope_impl(ExpiredCleanupScope::All { actor: principal.to_owned() }, audit).await
    }

    async fn evict_expired_with_scope_impl(&self, scope: ExpiredCleanupScope, audit: &AuditDraft) -> Result<u64, StoreError> {
        let now = self.clock_now().to_rfc3339();
        let audit = audit.clone();
        self.with_conn(move |conn| evict_expired_conn(conn, &now, &scope, &audit)).await
    }

    pub(crate) async fn set_embedding_impl(&self, id: &MemoryId, embedding: &[f32], expected_revision: i64) -> Result<(), StoreError> {
        let expected_dims = self.embedding_dimensions();
        validate_embedding_vector(embedding, expected_dims)?;

        let id_str = id.to_string();
        let emb = embedding.to_vec();
        let vector_index = self.vector_index();
        let active_profile = self.active_embedding_profile();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;

            if let Some(profile) = &active_profile {
                super::sqlite::ensure_embedding_profile_matches(&tx, profile)?;
            }

            let current_revision: Option<i64> = tx
                .query_row("SELECT embedding_revision FROM memories WHERE id = ?1", params![id_str], |row| row.get(0))
                .optional()?;
            let Some(current_revision) = current_revision else {
                return Err(StoreError::NotFound(format!("memory not found: {id_str}")));
            };
            if current_revision != expected_revision {
                return Err(StoreError::Conflict(format!(
                    "embedding revision mismatch for {id_str}: expected {expected_revision}, current {current_revision}"
                )));
            }

            vector_index.upsert(&tx, &id_str, &emb)?;

            // Atomically mark embedding as present — guards against concurrent
            // revision bumps between our initial check and this UPDATE.
            let affected = tx.execute(
                "UPDATE memories SET has_embedding = 1, embedding_claimed_at = NULL, embedding_claim_token = NULL WHERE id = ?1 AND embedding_revision = ?2",
                params![id_str, expected_revision],
            )?;
            if affected == 0 {
                return Err(StoreError::Conflict(format!("embedding revision changed while writing embedding for {id_str}")));
            }

            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn reassign_scope_impl(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
    ) -> Result<ReassignScopeOutcome, StoreError> {
        self.reassign_scope_audited_impl(from_scope, to_scope, origin_conversation, principal, None).await
    }

    #[expect(clippy::too_many_arguments, reason = "audited reassign needs scope pair, optional origin, principal, and audit draft")]
    pub(crate) async fn reassign_scope_audited_impl(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<ReassignScopeOutcome, StoreError> {
        let from_scope = from_scope.to_owned();
        let to_scope = to_scope.to_owned();
        let origin_conversation = origin_conversation.map(str::to_owned);
        let principal = principal.to_owned();
        let now = self.clock_now().to_rfc3339();
        let audit = audit.cloned();
        self.with_conn(move |conn| {
            apply_reassign_scope(conn, ReassignScopeApply {
                from_scope: &from_scope,
                to_scope: &to_scope,
                origin_conversation: origin_conversation.as_deref(),
                principal: &principal,
                now: &now,
                audit: audit.as_ref(),
            })
        })
        .await
    }

    pub(crate) async fn count_impl(&self, filter: MemoryFilter, ctx: QueryContext, top_tags_limit: usize) -> Result<MemoryStats, StoreError> {
        let filter = normalize_filter(filter);
        let principal = ctx.principal;
        let now = self.clock_now();
        self.with_conn(move |conn| count_with_access_filter(&*conn, &filter, principal.as_deref(), now, top_tags_limit))
            .await
    }

    pub(crate) async fn list_impl(&self, filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<Memory>, StoreError> {
        let filter = normalize_filter(filter);
        let principal = ctx.principal;
        let now = self.clock_now();
        self.with_conn(move |conn| list_with_paging(conn, &filter, principal.as_deref(), now)).await
    }

    pub(crate) async fn register_scope_impl(&self, scope: ScopeDefinition) -> Result<(), StoreError> {
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| register_legacy_scope_context(conn, &scope, &now, OPERATOR_PRINCIPAL)).await
    }

    pub(crate) async fn list_scopes_impl(&self) -> Result<Vec<ScopeDefinition>, StoreError> {
        self.with_conn(move |conn| list_legacy_scope_contexts(conn, OPERATOR_PRINCIPAL)).await
    }

    pub(crate) async fn register_scope_for_principal_impl(&self, scope: ScopeDefinition, principal: &str) -> Result<(), StoreError> {
        let now = self.clock_now().to_rfc3339();
        let principal = principal.to_owned();
        self.with_conn(move |conn| register_legacy_scope_context(conn, &scope, &now, &principal)).await
    }

    pub(crate) async fn list_scopes_for_principal_impl(&self, principal: &str) -> Result<Vec<ScopeDefinition>, StoreError> {
        let principal = principal.to_owned();
        self.with_conn(move |conn| list_legacy_scope_contexts(conn, &principal)).await
    }

    pub(crate) async fn upsert_metadata_impl(&self, metadata: MemoryMetadata) -> Result<(), StoreError> {
        self.upsert_metadata_audited_impl(metadata, None).await
    }

    pub(crate) async fn upsert_metadata_audited_impl(&self, metadata: MemoryMetadata, audit: Option<&AuditDraft>) -> Result<(), StoreError> {
        let now = self.clock_now();
        let audit = audit.cloned();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let id = metadata.memory_id;
            let id_str = id.to_string();
            let existing = fetch_memory_by_id(&tx, &id_str)?.ok_or_else(|| StoreError::NotFound(format!("memory not found: {id}")))?;
            let expected_scope = tx
                .query_row(
                    "SELECT context_row.context_key
                     FROM memory_contexts AS membership
                     JOIN contexts AS context_row ON context_row.id = membership.context_id
                     WHERE membership.memory_id = ?1
                       AND membership.ordinal = 0",
                    [&id_str],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_else(|| UNRESOLVED_SCOPE.to_owned());
            if metadata.scope_key.as_deref() != Some(expected_scope.as_str()) || existing.provenance.source_conversation.as_deref() != Some(expected_scope.as_str()) {
                return Err(StoreError::Conflict(
                    "metadata and provenance compatibility scopes must match the memory's primary governed context; replace governed memberships instead".into(),
                ));
            }
            upsert_metadata_conn(&tx, &metadata, &now.to_rfc3339())?;
            let affected = tx.execute("UPDATE memories SET record_revision = record_revision + 1 WHERE id = ?1", params![id_str])?;
            if affected == 0 {
                return Err(StoreError::Conflict(format!("memory {id} changed while updating metadata")));
            }
            insert_optional_metadata_audit(&tx, &id, audit.as_ref())?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn get_metadata_impl(&self, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, StoreError> {
        let memory_id_value = *memory_id;
        self.with_conn(move |conn| get_metadata_conn(conn, &memory_id_value)).await
    }

    pub(crate) async fn get_metadata_batch_impl(&self, memory_ids: &[MemoryId]) -> Result<HashMap<MemoryId, MemoryMetadata>, StoreError> {
        if memory_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids_json = serde_json::to_string(&memory_ids.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT memory_id, scope_key, summary, agent_label,
                        created_by_principal, quality_flags, schema_version
                 FROM memory_metadata
                 WHERE memory_id IN (SELECT value FROM json_each(?1))",
            )?;
            let rows = statement.query_map([ids_json], |row| {
                let id_str: String = row.get(0)?;
                let quality_flags_json: String = row.get(5)?;
                let memory_id = id_str
                    .parse()
                    .map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?;
                let quality_flags =
                    serde_json::from_str(&quality_flags_json).map_err(|error| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error)))?;
                Ok(MemoryMetadata {
                    memory_id,
                    scope_key: row.get(1)?,
                    summary: row.get(2)?,
                    agent_label: row.get(3)?,
                    created_by_principal: row.get(4)?,
                    quality_flags,
                    schema_version: row.get(6)?,
                })
            })?;
            let metadata = rows.collect::<Result<Vec<_>, _>>()?;
            Ok(metadata.into_iter().map(|record| (record.memory_id, record)).collect())
        })
        .await
    }

    pub(crate) async fn metadata_migration_report_impl(&self) -> Result<MetadataMigrationReport, StoreError> {
        let oversized_threshold = i64::try_from(LARGE_CONTENT_WARNING_THRESHOLD_BYTES).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        self.with_conn(move |conn| {
            let total_memories = conn.query_row("SELECT COUNT(*) FROM memories", [], sqlite_count)?;
            let metadata_rows = conn.query_row("SELECT COUNT(*) FROM memory_metadata", [], sqlite_count)?;
            let missing_metadata = total_memories.saturating_sub(metadata_rows);
            let missing_summary: u64 = conn.query_row(
                "SELECT COUNT(*)
                 FROM memories AS m
                 LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
                 WHERE meta.summary IS NULL OR trim(meta.summary) = ''",
                [],
                sqlite_count,
            )?;
            let unresolved_scope: u64 = conn.query_row(
                "SELECT COUNT(*)
                 FROM memories AS m
                 LEFT JOIN memory_contexts AS primary_membership
                   ON primary_membership.memory_id = m.id
                  AND primary_membership.ordinal = 0
                 LEFT JOIN contexts AS primary_context
                   ON primary_context.id = primary_membership.context_id
                 WHERE primary_context.context_key IS NULL
                    OR primary_context.context_key = ?1",
                [UNRESOLVED_SCOPE],
                sqlite_count,
            )?;
            let duplicate_candidates: u64 = conn.query_row(
                "SELECT COALESCE(SUM(cnt - 1), 0)
                 FROM (
                    SELECT COUNT(*) AS cnt
                    FROM memories
                    GROUP BY content
                    HAVING COUNT(*) > 1
                 )",
                [],
                sqlite_count,
            )?;
            let oversized: u64 = conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE length(CAST(content AS BLOB)) > ?1",
                params![oversized_threshold],
                sqlite_count,
            )?;
            let code_derived: u64 = conn.query_row(
                "SELECT COUNT(*)
                 FROM memories
                 WHERE content LIKE '%```%'
                    OR content LIKE '%fn %'
                    OR content LIKE '%function %'
                    OR content LIKE '%class %'
                    OR content LIKE '%use %;%'",
                [],
                sqlite_count,
            )?;
            Ok(MetadataMigrationReport {
                total_memories,
                metadata_rows,
                missing_metadata,
                missing_summary,
                unresolved_scope,
                duplicate_candidates,
                oversized,
                code_derived,
            })
        })
        .await
    }

    pub(crate) async fn migrate_metadata_impl(&self, dry_run: bool) -> Result<MetadataMigrationOutcome, StoreError> {
        self.migrate_metadata_audited_impl(dry_run, None).await
    }

    pub(crate) async fn migrate_metadata_audited_impl(&self, dry_run: bool, audit: Option<&AuditDraft>) -> Result<MetadataMigrationOutcome, StoreError> {
        let now = self.clock_now().to_rfc3339();
        let audit = audit.cloned();
        self.with_conn(move |conn| {
            if dry_run {
                return Ok(prepare_metadata_migration(conn)?.report);
            }
            let tx = sqlite_write_tx(conn)?;
            let mut preparation = prepare_metadata_migration(&tx)?;
            preparation.report.migrated = insert_metadata_migration_rows(&tx, &preparation.rows, &now, audit.as_ref())?;
            tx.commit()?;
            Ok(preparation.report)
        })
        .await
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "legacy adapter validates exact reuse and transactionally audits creation"
)]
fn ensure_private_legacy_scope_context(
    tx: &Transaction<'_>,
    key: &str,
    display_name: Option<&str>,
    description: Option<&str>,
    principal: &str,
    now: &str,
) -> Result<String, StoreError> {
    let normalized = normalize_context_key(key);
    if normalized.is_empty() || normalized == UNRESOLVED_SCOPE {
        return Err(StoreError::Conflict("legacy scope key cannot be blank or inbox/unresolved".into()));
    }
    let existing = tx
        .query_row(
            "SELECT id, kind, frozen, lifecycle
             FROM contexts
             WHERE owner_principal = ?1
               AND normalized_key = ?2
               AND kind = 'custom'",
            params![principal, normalized],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?, row.get::<_, String>(3)?)),
        )
        .optional()?;
    if let Some((id, kind, frozen, lifecycle)) = existing {
        if frozen || kind != ContextKind::CUSTOM {
            return Err(StoreError::Conflict(
                "legacy scope administration can update only principal-owned mutable custom contexts".into(),
            ));
        }
        if lifecycle != "active" {
            return Err(StoreError::Conflict(
                "legacy scope is archived; reactivate it in the TUI before using legacy administration".into(),
            ));
        }
        if let Some(display_name) = display_name {
            let _updated = tx.execute("UPDATE contexts SET display_name = ?1, description = ?2, updated_at = ?3 WHERE id = ?4", params![
                display_name,
                description,
                now,
                id
            ])?;
        }
        return Ok(id);
    }
    let visible_foreign: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM contexts AS context_row
             LEFT JOIN context_grants AS grant_row
               ON grant_row.context_id = context_row.id
              AND grant_row.grantee_principal IN (?1, ?3)
             WHERE context_row.normalized_key = ?2
               AND context_row.lifecycle = 'active'
               AND (context_row.owner_principal = ?1 OR grant_row.context_id IS NOT NULL)
         )",
        params![principal, normalized, LEGACY_ALL_PRINCIPALS_GRANT],
        |row| row.get(0),
    )?;
    if visible_foreign {
        return Err(StoreError::Conflict(
            "legacy scope key already belongs to another visible governed context and cannot be overridden".into(),
        ));
    }
    let id = ContextId::new().to_string();
    let fallback_display = key.rsplit('/').find(|part| !part.is_empty()).unwrap_or(key);
    let _inserted = tx.execute(
        "INSERT INTO contexts (
            id, kind, context_key, normalized_key, display_name, description,
            owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'active', 0, ?9, ?9)",
        params![
            id,
            ContextKind::CUSTOM,
            key.trim(),
            normalized,
            display_name.unwrap_or(fallback_display),
            description.or(Some("Private compatibility context created by legacy scope administration")),
            principal,
            "Migrate this workflow to governed context IDs.",
            now,
        ],
    )?;
    let _audited = tx.execute(
        "INSERT INTO context_audit_events (
            actor_principal, action, context_id, memory_id, timestamp, details
         ) VALUES (?1, 'legacy_scope_context_created', ?2, NULL, ?3, ?4)",
        params![principal, id, now, serde_json::json!({"legacy_scope_key": key}).to_string(),],
    )?;
    Ok(id)
}

fn resolve_or_create_private_legacy_context(tx: &Transaction<'_>, key: &str, principal: &str, now: &str) -> Result<String, StoreError> {
    validate_implicit_legacy_context_key(key).map_err(StoreError::Conflict)?;
    let normalized = normalize_context_key(key);
    if normalized.is_empty() || normalized == UNRESOLVED_SCOPE {
        return Err(StoreError::Conflict("legacy scope key cannot be blank or inbox/unresolved".into()));
    }
    let owned = tx
        .query_row(
            "SELECT id, lifecycle
             FROM contexts
             WHERE owner_principal = ?1
               AND kind = 'custom'
               AND normalized_key = ?2",
            params![principal, normalized],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((id, lifecycle)) = owned {
        if lifecycle == "active" {
            return Ok(id);
        }
        return Err(StoreError::Conflict(
            "legacy scope is archived; reactivate it in the TUI before using legacy administration".into(),
        ));
    }
    let mut statement = tx.prepare(
        "SELECT DISTINCT context_row.id
         FROM contexts AS context_row
         LEFT JOIN context_grants AS grant_row
           ON grant_row.context_id = context_row.id
          AND grant_row.grantee_principal IN (?1, ?3)
         WHERE context_row.normalized_key = ?2
           AND context_row.lifecycle = 'active'
           AND (context_row.owner_principal = ?1 OR grant_row.context_id IS NOT NULL)
         ORDER BY context_row.id",
    )?;
    let matches = statement
        .query_map(params![principal, normalized, LEGACY_ALL_PRINCIPALS_GRANT], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    match matches.as_slice() {
        [id] => return Ok(id.clone()),
        [_, _, ..] => {
            return Err(StoreError::Conflict(format!(
                "legacy scope {key:?} matches multiple governed contexts; select an exact context before reassigning"
            )));
        }
        [] => {}
    }

    let id = ContextId::new().to_string();
    let display_name = key.rsplit('/').find(|part| !part.is_empty()).unwrap_or(key);
    let _inserted = tx.execute(
        "INSERT INTO contexts (
            id, kind, context_key, normalized_key, display_name, description,
            owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'active', 0, ?9, ?9)",
        params![
            id,
            ContextKind::CUSTOM,
            key.trim(),
            normalized,
            display_name,
            "Private compatibility context created by legacy scope administration",
            principal,
            "Migrate this workflow to governed context IDs.",
            now,
        ],
    )?;
    let _audit = tx.execute(
        "INSERT INTO context_audit_events (
            actor_principal, action, context_id, memory_id, timestamp, details
         ) VALUES (?1, 'legacy_scope_context_created', ?2, NULL, ?3, ?4)",
        params![principal, id, now, serde_json::json!({"legacy_scope_key": key}).to_string(),],
    )?;
    Ok(id)
}

#[expect(clippy::too_many_lines, reason = "legacy adapter atomically synchronizes one compatibility context and its relationships")]
fn register_legacy_scope_context(conn: &mut Connection, scope: &ScopeDefinition, now: &str, principal: &str) -> Result<(), StoreError> {
    validate_legacy_scope_definition(scope).map_err(StoreError::Conflict)?;
    let tx = sqlite_write_tx(conn)?;
    let context_id = ensure_private_legacy_scope_context(&tx, &scope.scope_key, Some(&scope.display_name), scope.description.as_deref(), principal, now)?;

    let parent_id = scope
        .parent
        .as_deref()
        .map(|key| ensure_private_legacy_scope_context(&tx, key, None, Some("Legacy compatibility scope"), principal, now))
        .transpose()?;
    if parent_id.as_deref() == Some(context_id.as_str()) {
        return Err(StoreError::Conflict("a legacy compatibility scope cannot be its own parent".into()));
    }
    if let Some(parent_id) = &parent_id {
        let cycle: bool = tx.query_row(
            "WITH RECURSIVE ancestors(id, parent_id) AS (
                SELECT id, parent_id FROM contexts WHERE id = ?1
                UNION
                SELECT parent.id, parent.parent_id
                FROM contexts AS parent
                JOIN ancestors ON parent.id = ancestors.parent_id
             )
             SELECT EXISTS(SELECT 1 FROM ancestors WHERE id = ?2)",
            params![parent_id, context_id],
            |row| row.get(0),
        )?;
        if cycle {
            return Err(StoreError::Conflict(format!(
                "legacy scope {:?} parent would create a context hierarchy cycle",
                scope.scope_key
            )));
        }
    }
    let _parent_updated = tx.execute("UPDATE contexts SET parent_id = ?1, updated_at = ?2 WHERE id = ?3", params![parent_id, now, context_id])?;

    let _aliases_removed = tx.execute("DELETE FROM context_aliases WHERE context_id = ?1", [&context_id])?;
    let mut aliases = HashSet::new();
    for alias in &scope.aliases {
        let normalized = normalize_context_key(alias);
        if !normalized.is_empty() && aliases.insert(normalized.clone()) {
            let _inserted = tx.execute(
                "INSERT INTO context_aliases (context_id, alias, normalized_alias, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![context_id, alias, normalized, now],
            )?;
        }
    }

    let _hints_removed = tx.execute("DELETE FROM context_resolver_hints WHERE context_id = ?1", [&context_id])?;
    let mut hints = HashSet::new();
    for hint in &scope.matchers {
        let normalized = normalize_context_key(hint);
        if !normalized.is_empty() && hints.insert(normalized.clone()) {
            let _inserted = tx.execute(
                "INSERT INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![context_id, hint, normalized, now],
            )?;
        }
    }

    let _relations_removed = tx.execute(
        "DELETE FROM context_relations
         WHERE from_context_id = ?1 AND relation = 'legacy_related'",
        [&context_id],
    )?;
    for related in &scope.related {
        let related_id = ensure_private_legacy_scope_context(&tx, related, None, Some("Legacy compatibility scope"), principal, now)?;
        if related_id != context_id {
            let _inserted = tx.execute(
                "INSERT OR IGNORE INTO context_relations (
                    from_context_id, to_context_id, relation, created_at
                 ) VALUES (?1, ?2, 'legacy_related', ?3)",
                params![context_id, related_id, now],
            )?;
        }
    }

    let _audited = tx.execute(
        "INSERT INTO context_audit_events (
            actor_principal, action, context_id, memory_id, timestamp, details
         ) VALUES (?1, 'legacy_scope_register', ?2, NULL, ?3, ?4)",
        params![principal, context_id, now, serde_json::json!({"legacy_scope_key": scope.scope_key}).to_string(),],
    )?;
    tx.commit()?;
    Ok(())
}

fn list_legacy_scope_contexts(conn: &Connection, principal: &str) -> Result<Vec<ScopeDefinition>, StoreError> {
    let base_rows = {
        let mut statement = conn.prepare(
            "SELECT context_row.id, context_row.context_key, context_row.display_name,
                    context_row.description, parent.context_key
             FROM contexts AS context_row
             LEFT JOIN contexts AS parent ON parent.id = context_row.parent_id
             WHERE (
                    (context_row.owner_principal = ?2 AND context_row.kind = 'custom' AND context_row.frozen = 0)
                 OR (context_row.owner_principal = ?1 AND context_row.frozen = 1)
             )
               AND context_row.lifecycle = 'active'
               AND EXISTS (
                   SELECT 1 FROM context_audit_events AS audit
                   WHERE audit.context_id = context_row.id
                     AND audit.action IN ('migrate_registered_scope', 'legacy_scope_register')
               )
             ORDER BY context_row.normalized_key, context_row.id",
        )?;
        statement
            .query_map(params![LEGACY_SYSTEM_PRINCIPAL, principal], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut aliases_statement = conn.prepare("SELECT alias FROM context_aliases WHERE context_id = ?1 ORDER BY normalized_alias")?;
    let mut matchers_statement = conn.prepare("SELECT hint FROM context_resolver_hints WHERE context_id = ?1 ORDER BY normalized_hint")?;
    let mut related_statement = conn.prepare(
        "SELECT related.context_key
         FROM context_relations AS relation
         JOIN contexts AS related ON related.id = relation.to_context_id
         WHERE relation.from_context_id = ?1 AND relation.relation = 'legacy_related'
         ORDER BY related.normalized_key",
    )?;
    let mut scopes = Vec::with_capacity(base_rows.len());
    for (id, scope_key, display_name, description, parent) in base_rows {
        let aliases = aliases_statement.query_map([&id], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
        let matchers = matchers_statement.query_map([&id], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
        let related = related_statement.query_map([&id], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
        scopes.push(ScopeDefinition {
            scope_key,
            display_name,
            description,
            aliases,
            matchers,
            parent,
            related,
        });
    }
    Ok(scopes)
}

struct MigrationCandidate {
    id: String,
    content: String,
    source_agent: Option<String>,
    primary_context_key: Option<String>,
}

struct PreparedMigrationMetadata {
    id: String,
    scope_key: String,
    agent_label: Option<String>,
    unresolved_scope: bool,
    oversized: bool,
    code_derived: bool,
}

struct MetadataMigrationPreparation {
    rows: Vec<PreparedMigrationMetadata>,
    report: MetadataMigrationOutcome,
}

fn load_metadata_migration_candidates(conn: &Connection) -> Result<Vec<MigrationCandidate>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT
            m.id,
            m.content,
            json_extract(m.provenance, '$.source_agent') AS source_agent,
            primary_context.context_key AS primary_context_key
         FROM memories AS m
         LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
         LEFT JOIN memory_contexts AS primary_membership
           ON primary_membership.memory_id = m.id
          AND primary_membership.ordinal = 0
         LEFT JOIN contexts AS primary_context
           ON primary_context.id = primary_membership.context_id
         WHERE meta.memory_id IS NULL
         ORDER BY m.created_at, m.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MigrationCandidate {
            id: row.get(0)?,
            content: row.get(1)?,
            source_agent: row.get(2)?,
            primary_context_key: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn prepare_metadata_migration_metadata(candidate: MigrationCandidate) -> PreparedMigrationMetadata {
    let scope_key = candidate.primary_context_key.unwrap_or_else(|| UNRESOLVED_SCOPE.to_owned());
    let unresolved_scope = scope_key == UNRESOLVED_SCOPE;
    let oversized = candidate.content.len() > LARGE_CONTENT_WARNING_THRESHOLD_BYTES;
    let code_derived = looks_code_derived(&candidate.content);

    PreparedMigrationMetadata {
        id: candidate.id,
        scope_key,
        agent_label: candidate.source_agent,
        unresolved_scope,
        oversized,
        code_derived,
    }
}

fn prepare_metadata_migration(conn: &Connection) -> Result<MetadataMigrationPreparation, StoreError> {
    let skipped_existing = conn.query_row("SELECT COUNT(*) FROM memory_metadata", [], sqlite_count)?;
    let candidates = load_metadata_migration_candidates(conn)?;
    let candidate_count = u64::try_from(candidates.len()).map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let prepared_rows = candidates.into_iter().map(prepare_metadata_migration_metadata).collect::<Vec<_>>();
    let report = metadata_migration_outcome(candidate_count, skipped_existing, &prepared_rows);
    Ok(MetadataMigrationPreparation { rows: prepared_rows, report })
}

fn metadata_migration_outcome(candidate_count: u64, skipped_existing: u64, prepared_rows: &[PreparedMigrationMetadata]) -> MetadataMigrationOutcome {
    MetadataMigrationOutcome {
        candidate_count,
        skipped_existing,
        migrated: 0,
        unresolved_scope: count_prepared_rows(prepared_rows, |row| row.unresolved_scope),
        missing_summary: candidate_count,
        oversized: count_prepared_rows(prepared_rows, |row| row.oversized),
        code_derived: count_prepared_rows(prepared_rows, |row| row.code_derived),
    }
}

fn count_prepared_rows(prepared_rows: &[PreparedMigrationMetadata], predicate: impl Fn(&PreparedMigrationMetadata) -> bool) -> u64 {
    prepared_rows.iter().filter(|row| predicate(row)).count().try_into().unwrap_or(u64::MAX)
}

fn insert_optional_metadata_audit(conn: &Connection, memory_id: &MemoryId, audit: Option<&AuditDraft>) -> Result<(), StoreError> {
    if let Some(audit) = audit {
        insert_audit_draft(conn, memory_id, audit)?;
    }
    Ok(())
}

fn insert_metadata_migration_rows(tx: &Transaction<'_>, prepared_rows: &[PreparedMigrationMetadata], now: &str, audit: Option<&AuditDraft>) -> Result<u64, StoreError> {
    let mut migrated = 0_u64;
    for row in prepared_rows {
        let quality_flags_json = serde_json::to_string(&migration_quality_flags(row.unresolved_scope, row.oversized, row.code_derived))?;
        let inserted = tx.execute(
            "INSERT INTO memory_metadata (
                memory_id, scope_key, summary, agent_label, created_by_principal,
                quality_flags, schema_version, migrated_at, updated_at
             ) VALUES (?1, ?2, NULL, ?3, NULL, ?4, 1, ?5, ?5)
             ON CONFLICT(memory_id) DO NOTHING",
            params![row.id, row.scope_key, row.agent_label, quality_flags_json, now],
        )?;
        if inserted > 0 {
            let memory_id = row.id.parse().map_err(|e| StoreError::Serialization(format!("invalid memory id: {e}").into()))?;
            let revised = tx.execute("UPDATE memories SET record_revision = record_revision + 1 WHERE id = ?1", params![row.id])?;
            if revised == 0 {
                return Err(StoreError::Conflict(format!("memory {memory_id} changed while migrating metadata")));
            }
            insert_optional_metadata_audit(tx, &memory_id, audit)?;
        }
        migrated = migrated.saturating_add(u64::try_from(inserted).map_err(|e| StoreError::Serialization(Box::new(e)))?);
    }
    Ok(migrated)
}

fn migration_quality_flags(unresolved_scope: bool, oversized: bool, code_derived: bool) -> Vec<String> {
    let mut flags = vec!["missing_summary".to_owned()];
    if unresolved_scope {
        flags.push("missing_scope".to_owned());
    }
    if oversized {
        flags.push("oversized_content".to_owned());
    }
    if code_derived {
        flags.push("possible_code_dump".to_owned());
    }
    flags
}

fn looks_code_derived(content: &str) -> bool {
    content.contains("```")
        || content
            .lines()
            .take(20)
            .any(|line| line.trim_start().starts_with("fn ") || line.trim_start().starts_with("impl "))
}

fn list_with_paging(conn: &Connection, filter: &MemoryFilter, caller: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<Memory>, StoreError> {
    let limit = filter.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut results: Vec<Memory> = Vec::with_capacity(limit);
    let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);

    ScanConfig::new(conn, filter, caller, now, page_size).run_hydrated(|memory| {
        let Some(m) = memory.apply_access_policy(caller) else {
            return true; // denied — skip but continue
        };
        results.push(m);
        results.len() < limit
    })?;

    Ok(results)
}

#[derive(Clone, Copy)]
struct ReassignScopeApply<'a> {
    from_scope: &'a str,
    to_scope: &'a str,
    origin_conversation: Option<&'a str>,
    principal: &'a str,
    now: &'a str,
    audit: Option<&'a AuditDraft>,
}

#[expect(
    clippy::too_many_lines,
    reason = "scope reassignment keeps selection, authorization, metadata, and audit update in one transaction"
)]
fn apply_reassign_scope(conn: &mut Connection, params: ReassignScopeApply<'_>) -> Result<ReassignScopeOutcome, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let mut select_sql = "SELECT id FROM memories WHERE json_extract(provenance, '$.source_conversation') = ?1".to_owned();
    let mut select_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(params.from_scope.to_owned())];
    if let Some(origin) = params.origin_conversation {
        select_sql.push_str(" AND COALESCE(json_extract(provenance, '$.origin_conversation'), json_extract(provenance, '$.source_conversation')) = ?2");
        select_values.push(Box::new(origin.to_owned()));
    }
    let select_params: Vec<&dyn rusqlite::types::ToSql> = select_values.iter().map(AsRef::as_ref).collect();
    let mut select_stmt = tx.prepare(&select_sql)?;
    let applied_ids: Vec<MemoryId> = select_stmt
        .query_map(select_params.as_slice(), |row| row.get::<_, String>(0))?
        .map(|row| {
            let id_str = row?;
            id_str.parse().map_err(|e| StoreError::Serialization(format!("invalid memory id: {e}").into()))
        })
        .collect::<Result<_, _>>()?;
    drop(select_stmt);

    let mut authorized_ids: Vec<MemoryId> = Vec::new();
    for id in &applied_ids {
        let id_str = id.to_string();
        let Some(memory) = fetch_memory_by_id(&tx, &id_str)? else {
            continue;
        };
        if memory.has_write_access(params.principal) {
            authorized_ids.push(*id);
        }
    }

    if authorized_ids.is_empty() {
        tx.commit()?;
        return Ok(ReassignScopeOutcome { applied_ids: authorized_ids });
    }

    let target_context_id = resolve_or_create_private_legacy_context(&tx, params.to_scope, params.principal, params.now)?;
    let target_scope_key: String = tx.query_row("SELECT context_key FROM contexts WHERE id = ?1", [&target_context_id], |row| row.get(0))?;

    let mut updated = 0_usize;
    for chunk in authorized_ids.chunks(SQLITE_MAX_CHUNK) {
        let placeholder_end = chunk.len().saturating_add(1);
        let placeholders: Vec<String> = (2..=placeholder_end).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "UPDATE memories \
             SET provenance = json_set( \
                 json_set( \
                     provenance, \
                     '$.origin_conversation', \
                     COALESCE(json_extract(provenance, '$.origin_conversation'), json_extract(provenance, '$.source_conversation')) \
                 ), \
                 '$.source_conversation', \
                 ?1 \
            ) \
             WHERE id IN ({})",
            placeholders.join(", ")
        );
        let id_strings: Vec<String> = chunk.iter().map(ToString::to_string).collect();
        let mut memory_params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(chunk.len().saturating_add(1));
        memory_params.push(&target_scope_key);
        for id in &id_strings {
            memory_params.push(id);
        }
        updated = updated.saturating_add(tx.execute(&sql, memory_params.as_slice())?);
        for id in chunk {
            let id_str = id.to_string();
            let affected = tx.execute("UPDATE memories SET record_revision = record_revision + 1 WHERE id = ?1", params![id_str])?;
            if affected == 0 {
                return Err(StoreError::Conflict(format!("memory {id} changed while reassigning scope")));
            }
        }

        let metadata_placeholders: Vec<String> = (3..=chunk.len().saturating_add(2)).map(|i| format!("?{i}")).collect();
        let metadata_sql = format!(
            "UPDATE memory_metadata \
             SET scope_key = ?1, updated_at = ?2 \
             WHERE memory_id IN ({})",
            metadata_placeholders.join(", ")
        );
        let mut metadata_params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(chunk.len().saturating_add(2));
        metadata_params.push(&target_scope_key);
        metadata_params.push(&params.now);
        for id in &id_strings {
            metadata_params.push(id);
        }
        #[expect(unused_results, reason = "not every reassigned memory has metadata yet")]
        tx.execute(&metadata_sql, metadata_params.as_slice())?;
        for id in chunk {
            let id_str = id.to_string();
            let mut membership_statement = tx.prepare("SELECT context_id FROM memory_contexts WHERE memory_id = ?1 ORDER BY ordinal, context_id")?;
            let mut context_ids = membership_statement.query_map([&id_str], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            drop(membership_statement);
            if context_ids.is_empty() {
                context_ids.push(target_context_id.clone());
            } else {
                context_ids[0].clone_from(&target_context_id);
                let mut seen = HashSet::new();
                context_ids.retain(|context_id| seen.insert(context_id.clone()));
            }
            let _removed = tx.execute("DELETE FROM memory_contexts WHERE memory_id = ?1", [&id_str])?;
            for (ordinal, context_id) in context_ids.iter().enumerate() {
                let ordinal = i64::try_from(ordinal).map_err(|error| StoreError::Conflict(format!("context membership ordinal exceeds SQLite INTEGER: {error}")))?;
                let _inserted = tx.execute(
                    "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id_str, context_id, ordinal, params.now],
                )?;
            }
            let _context_audit = tx.execute(
                "INSERT INTO context_audit_events (
                    actor_principal, action, context_id, memory_id, timestamp, details
                 ) VALUES (?1, 'legacy_scope_reassigned', ?2, ?3, ?4, ?5)",
                params![
                    params.principal,
                    target_context_id,
                    id_str,
                    params.now,
                    serde_json::json!({
                        "from_scope": params.from_scope,
                        "to_scope": target_scope_key,
                        "preserved_companion_contexts": context_ids.len().saturating_sub(1),
                    })
                    .to_string(),
                ],
            )?;
        }
        if let Some(audit) = params.audit {
            for id in chunk {
                insert_audit_draft(&tx, id, audit)?;
            }
        }
    }
    debug_assert_eq!(updated, authorized_ids.len(), "reassign_scope should update exactly the authorized rows");
    tx.commit()?;
    Ok(ReassignScopeOutcome { applied_ids: authorized_ids })
}

fn evict_expired_conn(conn: &mut Connection, now: &str, scope: &ExpiredCleanupScope, audit: &AuditDraft) -> Result<u64, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let mut stmt = tx.prepare(
        "SELECT id, provenance, access_policy
         FROM memories
         WHERE expires_at IS NOT NULL AND expires_at <= ?1
           AND (
               ?2 IS NULL
               OR json_extract(provenance, '$.source_agent') = ?2
               OR (
                   json_extract(access_policy, '$.type') = 'public'
                   AND json_extract(provenance, '$.source_agent') IS NULL
               )
               OR (
                   json_extract(access_policy, '$.type') = 'restricted'
                   AND EXISTS (
                       SELECT 1
                       FROM json_each(access_policy, '$.allowed')
                       WHERE value = ?2
                   )
               )
           )
         ORDER BY expires_at ASC, id ASC",
    )?;
    let expired = stmt
        .query_and_then(params![now, scope.authorization_principal()], |row| {
            let id: String = row.get(0)?;
            Ok(MemoryAuthorizationEnvelope {
                id: id
                    .parse::<MemoryId>()
                    .map_err(|error| StoreError::Serialization(format!("invalid memory id: {error}").into()))?,
                provenance: serde_json::from_str::<Provenance>(&row.get::<_, String>(1)?)?,
                access_policy: serde_json::from_str::<AccessPolicy>(&row.get::<_, String>(2)?)?,
            })
        })?
        .collect::<Result<Vec<_>, StoreError>>()?;
    drop(stmt);

    let mut deleted = 0_usize;
    for memory in expired {
        // SQL narrows candidates efficiently; this shared Rust policy remains
        // the fail-closed authority if serialized representations evolve.
        if scope
            .authorization_principal()
            .is_some_and(|authorized_principal| !memory.has_write_access(authorized_principal))
        {
            continue;
        }
        insert_authorization_tombstone(&tx, MemoryAuthorizationRef::from(&memory), now, Some(scope.actor()))?;
        let affected = tx.execute("DELETE FROM memories WHERE id = ?1", params![memory.id.to_string()])?;
        if affected > 0 {
            insert_audit_draft(&tx, &memory.id, audit)?;
            deleted = deleted.saturating_add(affected);
        }
    }
    tx.commit()?;
    u64::try_from(deleted).map_err(|e| StoreError::Serialization(Box::new(e)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::{
        error::StoreError,
        store::SqliteStore,
        types::{AccessPolicy, Importance, Memory, MemoryId, MemoryType, Provenance},
    };

    fn make_memory(content: &str) -> Memory {
        Memory {
            id: MemoryId::new(),
            content: content.into(),
            tags: vec![],
            provenance: Provenance::default(),
            access_policy: AccessPolicy::Public,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            record_revision: 0_i64,
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::default(),
            confidence: crate::types::Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    // -- RR-124: set_embedding dimension mismatch error path -----------------

    #[tokio::test]
    async fn set_embedding_dimension_mismatch_returns_conflict() {
        use crate::store::MemoryWriter as _;

        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("embed me");
        let id = store.store(&mem, None).await.unwrap();

        // DEFAULT_TEST_DIMENSIONS is 768; use a 256-dim vector.
        let wrong_dim = vec![0.5_f32; 256];
        let err = store.set_embedding(&id, &wrong_dim, 0).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "expected Conflict, got: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("dimension mismatch"), "error should mention dimension mismatch: {msg}");
        assert!(msg.contains("768"), "error should mention expected dimensions: {msg}");
        assert!(msg.contains("256"), "error should mention actual dimensions: {msg}");
    }

    #[tokio::test]
    async fn set_embedding_zero_dim_returns_conflict() {
        use crate::store::MemoryWriter as _;

        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("embed me");
        let id = store.store(&mem, None).await.unwrap();

        let empty: Vec<f32> = vec![];
        let err = store.set_embedding(&id, &empty, 0).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "expected Conflict, got: {err:?}");
    }
}
