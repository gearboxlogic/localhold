//! Administrative operations — eviction, scope reassignment, embedding management, and statistics.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension as _, params};

use super::{
    ReassignScopeOutcome, SqliteStore,
    crud::{SQLITE_MAX_CHUNK, fetch_memory_by_id, get_metadata_conn, insert_audit_draft, insert_tombstone, upsert_metadata_conn},
    query::{DEFAULT_LIST_LIMIT, OVERFETCH_FACTOR, ScanConfig, count_with_access_filter, normalize_filter},
    sqlite_write_tx,
    vector::{VectorIndex as _, validate_embedding_vector},
};
use crate::{
    error::StoreError,
    types::{
        AuditDraft, LARGE_CONTENT_WARNING_THRESHOLD_BYTES, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryStats, MetadataMigrationOutcome, MetadataMigrationReport,
        QueryContext, ScopeDefinition,
    },
};

const UNRESOLVED_SCOPE: &str = "inbox/unresolved";

fn sqlite_count(row: &rusqlite::Row<'_>) -> rusqlite::Result<u64> {
    let count: i64 = row.get(0)?;
    u64::try_from(count).map_err(|_err| rusqlite::Error::IntegralValueOutOfRange(0, count))
}

#[expect(clippy::multiple_inherent_impl, reason = "SqliteStore methods are split across submodules by concern")]
impl SqliteStore {
    pub(crate) async fn evict_expired_impl(&self, principal: &str, audit: &AuditDraft) -> Result<u64, StoreError> {
        let now = self.clock_now().to_rfc3339();
        let principal = principal.to_owned();
        let audit = audit.clone();
        self.with_conn(move |conn| evict_expired_conn(conn, &now, &principal, &audit)).await
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
        self.with_conn(move |conn| {
            let aliases_json = serde_json::to_string(&scope.aliases)?;
            let matchers_json = serde_json::to_string(&scope.matchers)?;
            let related_json = serde_json::to_string(&scope.related)?;
            #[expect(unused_results, reason = "UPSERT row count is not needed")]
            conn.execute(
                "INSERT INTO scope_registry (
                    scope_key, display_name, description, aliases, matchers, parent, related, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(scope_key) DO UPDATE SET
                    display_name = excluded.display_name,
                    description = excluded.description,
                    aliases = excluded.aliases,
                    matchers = excluded.matchers,
                    parent = excluded.parent,
                    related = excluded.related,
                    updated_at = excluded.updated_at",
                params![
                    scope.scope_key,
                    scope.display_name,
                    scope.description,
                    aliases_json,
                    matchers_json,
                    scope.parent,
                    related_json,
                    now,
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn list_scopes_impl(&self) -> Result<Vec<ScopeDefinition>, StoreError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT scope_key, display_name, description, aliases, matchers, parent, related
                 FROM scope_registry
                 ORDER BY scope_key",
            )?;
            let scopes = stmt
                .query_map([], |row| {
                    let aliases_json: String = row.get(3)?;
                    let matchers_json: String = row.get(4)?;
                    let related_json: String = row.get(6)?;
                    let aliases = serde_json::from_str(&aliases_json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?;
                    let matchers = serde_json::from_str(&matchers_json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;
                    let related = serde_json::from_str(&related_json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e)))?;
                    Ok(ScopeDefinition {
                        scope_key: row.get(0)?,
                        display_name: row.get(1)?,
                        description: row.get(2)?,
                        aliases,
                        matchers,
                        parent: row.get(5)?,
                        related,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(scopes)
        })
        .await
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
            let _existing = fetch_memory_by_id(&tx, &id_str)?.ok_or_else(|| StoreError::NotFound(format!("memory not found: {id}")))?;
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
                 LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
                 WHERE COALESCE(meta.scope_key, json_extract(m.provenance, '$.source_conversation')) IS NULL
                    OR COALESCE(meta.scope_key, json_extract(m.provenance, '$.source_conversation')) = 'inbox/unresolved'",
                [],
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

    pub(crate) async fn migrate_metadata_impl(&self, registered_scope_keys: &[String], dry_run: bool) -> Result<MetadataMigrationOutcome, StoreError> {
        self.migrate_metadata_audited_impl(registered_scope_keys, dry_run, None).await
    }

    pub(crate) async fn migrate_metadata_audited_impl(
        &self,
        registered_scope_keys: &[String],
        dry_run: bool,
        audit: Option<&AuditDraft>,
    ) -> Result<MetadataMigrationOutcome, StoreError> {
        let registered_scope_keys = registered_scope_keys.iter().cloned().collect::<HashSet<_>>();
        let now = self.clock_now().to_rfc3339();
        let audit = audit.cloned();
        self.with_conn(move |conn| {
            let skipped_existing = conn.query_row("SELECT COUNT(*) FROM memory_metadata", [], sqlite_count)?;
            let candidates = load_metadata_migration_candidates(conn)?;
            let candidate_count = u64::try_from(candidates.len()).map_err(|e| StoreError::Serialization(Box::new(e)))?;
            let prepared_rows = candidates
                .into_iter()
                .map(|candidate| prepare_metadata_migration_metadata(candidate, &registered_scope_keys))
                .collect::<Vec<_>>();
            let mut report = metadata_migration_outcome(candidate_count, skipped_existing, &prepared_rows);

            if dry_run {
                return Ok(report);
            }
            report.migrated = insert_metadata_migration_rows(conn, &prepared_rows, &now, audit.as_ref())?;
            Ok(report)
        })
        .await
    }
}

struct MigrationCandidate {
    id: String,
    content: String,
    source_agent: Option<String>,
    source_conversation: Option<String>,
}

struct PreparedMigrationMetadata {
    id: String,
    scope_key: String,
    agent_label: Option<String>,
    quality_flags: Vec<String>,
    unresolved_scope: bool,
    oversized: bool,
    code_derived: bool,
}

fn load_metadata_migration_candidates(conn: &Connection) -> Result<Vec<MigrationCandidate>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT
            m.id,
            m.content,
            json_extract(m.provenance, '$.source_agent') AS source_agent,
            json_extract(m.provenance, '$.source_conversation') AS source_conversation
         FROM memories AS m
         LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
         WHERE meta.memory_id IS NULL
         ORDER BY m.created_at, m.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MigrationCandidate {
            id: row.get(0)?,
            content: row.get(1)?,
            source_agent: row.get(2)?,
            source_conversation: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn prepare_metadata_migration_metadata(candidate: MigrationCandidate, registered_scope_keys: &HashSet<String>) -> PreparedMigrationMetadata {
    let scope_key = candidate
        .source_conversation
        .as_deref()
        .filter(|scope| registered_scope_keys.contains(*scope))
        .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), ToOwned::to_owned);
    let unresolved_scope = scope_key == UNRESOLVED_SCOPE;
    let oversized = candidate.content.len() > LARGE_CONTENT_WARNING_THRESHOLD_BYTES;
    let code_derived = looks_code_derived(&candidate.content);

    PreparedMigrationMetadata {
        id: candidate.id,
        scope_key,
        agent_label: candidate.source_agent,
        quality_flags: migration_quality_flags(unresolved_scope, oversized, code_derived),
        unresolved_scope,
        oversized,
        code_derived,
    }
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

fn insert_metadata_migration_rows(conn: &mut Connection, prepared_rows: &[PreparedMigrationMetadata], now: &str, audit: Option<&AuditDraft>) -> Result<u64, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let mut migrated = 0_u64;
    for row in prepared_rows {
        let quality_flags_json = serde_json::to_string(&row.quality_flags)?;
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
            insert_optional_metadata_audit(&tx, &memory_id, audit)?;
        }
        migrated = migrated.saturating_add(u64::try_from(inserted).map_err(|e| StoreError::Serialization(Box::new(e)))?);
    }
    tx.commit()?;
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
        memory_params.push(&params.to_scope);
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
        metadata_params.push(&params.to_scope);
        metadata_params.push(&params.now);
        for id in &id_strings {
            metadata_params.push(id);
        }
        #[expect(unused_results, reason = "not every reassigned memory has metadata yet")]
        tx.execute(&metadata_sql, metadata_params.as_slice())?;
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

fn evict_expired_conn(conn: &mut Connection, now: &str, principal: &str, audit: &AuditDraft) -> Result<u64, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let mut stmt = tx.prepare(
        "SELECT id
         FROM memories
         WHERE expires_at IS NOT NULL AND expires_at <= ?1
         ORDER BY expires_at ASC, id ASC",
    )?;
    let expired_ids = stmt.query_map(params![now], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut deleted = 0_usize;
    for id in expired_ids {
        let Some(memory) = fetch_memory_by_id(&tx, &id)? else {
            continue;
        };
        insert_tombstone(&tx, &memory, now, Some(principal))?;
        let affected = tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
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
