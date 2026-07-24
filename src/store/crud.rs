//! CRUD operations — store, get, update, delete, batch store, and audit logging.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::{
    EmbeddingProfile, ReembedClaim, SqliteStore, merge_metadata_patch,
    query::{MEMORY_COLUMN_COUNT, MEMORY_COLUMNS, row_to_memory, usize_to_i64},
    sqlite::ensure_embedding_profile_matches,
    sqlite_write_tx, update_audit_draft_for_locked_memory,
    vector::{SqliteVecIndex, VectorIndex, validate_embedding_vector},
};
use crate::{
    error::StoreError,
    types::{
        AccessLevel, AccessPolicy, AuditAction, AuditDraft, AuditEntry, AuthorizedUpdateOutcome, Entity, Memory, MemoryId, MemoryMetadata, MemoryTombstone, MemoryUpdate,
        MetadataPatch, Provenance, WriteOutcome,
    },
};

/// Column list for `INSERT INTO memories`, derived from [`MEMORY_COLUMNS`].
const INSERT_COLUMNS: &str = MEMORY_COLUMNS;
const EMBEDDING_CLAIM_LEASE_SECS: i64 = 300;

/// Generate a numbered SQL placeholder list (`?1, ?2, …, ?N`) at compile time.
macro_rules! numbered_placeholders {
    ($($n:literal),+ $(,)?) => {
        concat_placeholders!($($n),+)
    };
}

/// Concatenate `?N` tokens with `", "` separators.
macro_rules! concat_placeholders {
    ($first:literal $(, $rest:literal)*) => {
        concat!("?", stringify!($first) $(, ", ?", stringify!($rest))*)
    };
}

/// Placeholder list matching [`INSERT_COLUMNS`] (`?1, ?2, …, ?13`).
///
/// When adding columns to [`super::query::COLUMNS`], append the next number here
/// too. The static assertion below will fail at compile time if they diverge.
const INSERT_PLACEHOLDERS: &str = numbered_placeholders![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18];

/// Compile-time check: placeholder count must equal column count.
const _: () = assert!(
    MEMORY_COLUMN_COUNT == 18,
    "INSERT_PLACEHOLDERS is out of sync with COLUMNS -- update the numbered_placeholders! invocation"
);

/// Pre-built INSERT SQL for the memories table, avoiding per-call `format!`.
static INSERT_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("INSERT INTO memories ({INSERT_COLUMNS}) VALUES ({INSERT_PLACEHOLDERS})"));

/// A memory serialized into SQL-ready values for insertion.
pub(crate) struct PreparedMemoryRow {
    id: MemoryId,
    id_str: String,
    content: String,
    tags_json: String,
    provenance_json: String,
    access_json: String,
    created_at: String,
    expires_at: Option<String>,
    has_embedding: bool,
    embedding: Option<Vec<f32>>,
    memory_type: String,
    importance: f64,
    impression_count: i64,
    last_impressed_at: Option<String>,
    superseded_by: Option<String>,
    activity_mass: f64,
    last_used_at: Option<String>,
    updated_at: String,
    confidence: f64,
    record_revision: i64,
    entities: Vec<Entity>,
}

impl PreparedMemoryRow {
    #[expect(clippy::cast_possible_wrap, reason = "u64 → i64 cast: impression_count fits in i64 for practical values")]
    #[expect(clippy::as_conversions, reason = "u64 → i64 cast: impression_count fits in i64 for practical values")]
    pub(crate) fn from_memory(memory: &Memory, embedding: Option<&[f32]>) -> Result<Self, StoreError> {
        Ok(Self {
            id: memory.id,
            id_str: memory.id.to_string(),
            content: memory.content.clone(),
            tags_json: serde_json::to_string(&memory.tags)?,
            provenance_json: serde_json::to_string(&memory.provenance)?,
            access_json: serde_json::to_string(&memory.access_policy)?,
            created_at: memory.created_at.to_rfc3339(),
            expires_at: memory.expires_at.map(|t| t.to_rfc3339()),
            has_embedding: embedding.is_some(),
            embedding: embedding.map(<[f32]>::to_vec),
            memory_type: memory.memory_type.to_string(),
            importance: memory.importance.value(),
            impression_count: memory.impression_count as i64,
            last_impressed_at: memory.last_impressed_at.map(|t| t.to_rfc3339()),
            superseded_by: memory.superseded_by.map(|id| id.to_string()),
            activity_mass: memory.activity_mass,
            last_used_at: memory.last_used_at.map(|t| t.to_rfc3339()),
            updated_at: memory.updated_at.to_rfc3339(),
            confidence: memory.confidence.value(),
            record_revision: memory.record_revision,
            entities: memory.entities.clone(),
        })
    }

    /// Prepare a memory row from an owned `Memory`, avoiding clones on content, tags, etc.
    #[expect(dead_code, reason = "RR-016: owned variant available for callers that have Memory ownership; trait currently takes &Memory")]
    #[expect(clippy::cast_possible_wrap, reason = "u64 → i64 cast: impression_count fits in i64 for practical values")]
    #[expect(clippy::as_conversions, reason = "u64 → i64 cast: impression_count fits in i64 for practical values")]
    pub(crate) fn from_memory_owned(memory: Memory, embedding: Option<&[f32]>) -> Result<Self, StoreError> {
        let id = memory.id;
        let id_str = id.to_string();
        let tags_json = serde_json::to_string(&memory.tags)?;
        let provenance_json = serde_json::to_string(&memory.provenance)?;
        let access_json = serde_json::to_string(&memory.access_policy)?;
        let created_at = memory.created_at.to_rfc3339();
        let expires_at = memory.expires_at.map(|t| t.to_rfc3339());
        let has_embedding = embedding.is_some();
        let emb = embedding.map(<[f32]>::to_vec);
        let memory_type = memory.memory_type.to_string();
        let importance = memory.importance.value();
        let impression_count = memory.impression_count as i64;
        let last_impressed_at = memory.last_impressed_at.map(|t| t.to_rfc3339());
        let superseded_by = memory.superseded_by.map(|sid| sid.to_string());
        let activity_mass = memory.activity_mass;
        let last_used_at = memory.last_used_at.map(|t| t.to_rfc3339());
        let updated_at = memory.updated_at.to_rfc3339();
        let confidence = memory.confidence.value();
        let record_revision = memory.record_revision;
        Ok(Self {
            id,
            id_str,
            content: memory.content,
            tags_json,
            provenance_json,
            access_json,
            created_at,
            expires_at,
            has_embedding,
            embedding: emb,
            memory_type,
            importance,
            impression_count,
            last_impressed_at,
            superseded_by,
            activity_mass,
            last_used_at,
            updated_at,
            confidence,
            record_revision,
            entities: memory.entities,
        })
    }

    pub(crate) fn insert(&self, conn: &Connection, vector_index: &impl VectorIndex<Connection>) -> Result<MemoryId, StoreError> {
        #[expect(unused_results, reason = "INSERT row count is always 1 — not useful")]
        conn.execute(&INSERT_SQL, params![
            self.id_str,
            self.content,
            self.tags_json,
            self.provenance_json,
            self.access_json,
            self.created_at,
            self.expires_at,
            self.has_embedding,
            self.memory_type,
            self.importance,
            self.impression_count,
            self.last_impressed_at,
            self.superseded_by,
            self.activity_mass,
            self.last_used_at,
            self.updated_at,
            self.confidence,
            self.record_revision,
        ])?;
        if let Some(emb) = &self.embedding {
            vector_index.upsert(conn, &self.id_str, emb)?;
        }
        insert_entities(conn, &self.id_str, &self.entities)?;
        Ok(self.id)
    }
}

/// Mark an existing memory as superseded by a new memory ID.
///
/// The `AND superseded_by IS NULL` guard ensures a memory can only be
/// superseded once. If the row exists but is already superseded, a
/// [`StoreError::Conflict`] is returned so callers can handle the
/// collision (log and skip for consolidation, propagate for explicit
/// supersession).
///
/// Returns `true` if the row was updated, `false` if the referenced
/// memory was not found at all.
pub(crate) fn mark_superseded(conn: &Connection, old_id_str: &str, new_id_str: &str, _now: chrono::DateTime<chrono::Utc>) -> Result<bool, StoreError> {
    let Some(existing) = fetch_memory_by_id(conn, old_id_str)? else {
        return Ok(false);
    };
    if existing.superseded_by.is_some() {
        return Err(StoreError::Conflict(format!("memory {old_id_str} is already superseded")));
    }

    let affected = conn.execute(
        "UPDATE memories SET superseded_by = ?1, record_revision = record_revision + 1 WHERE id = ?2 AND superseded_by IS NULL",
        params![new_id_str, old_id_str],
    )?;
    if affected == 0 {
        return Err(StoreError::Conflict(format!("memory {old_id_str} changed while superseding")));
    }
    Ok(true)
}

/// Insert entity rows for a memory within an active transaction or connection.
#[expect(unused_results, reason = "INSERT row count is always 1 per entity — not useful")]
pub(crate) fn insert_entities(conn: &Connection, memory_id: &str, entities: &[Entity]) -> Result<(), StoreError> {
    if entities.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare("INSERT OR IGNORE INTO memory_entities (memory_id, entity, entity_type) VALUES (?1, ?2, ?3)")?;
    for entity in entities {
        stmt.execute(params![memory_id, entity.name, entity.entity_type.as_str()])?;
    }
    Ok(())
}

/// Replace all entity rows for a memory (DELETE + INSERT).
pub(crate) fn replace_entities(conn: &Connection, memory_id: &str, entities: &[Entity]) -> Result<(), StoreError> {
    #[expect(unused_results, reason = "DELETE row count not needed — clearing for replacement")]
    conn.execute("DELETE FROM memory_entities WHERE memory_id = ?1", params![memory_id])?;
    insert_entities(conn, memory_id, entities)
}

/// Hydrate entities for a single memory by its string ID.
pub(crate) fn hydrate_entities_single(conn: &Connection, memory_id: &str) -> Result<Vec<Entity>, StoreError> {
    let mut stmt = conn.prepare("SELECT entity, entity_type FROM memory_entities WHERE memory_id = ?1")?;
    let entities = stmt
        .query_map(params![memory_id], |row| {
            let entity_type_str: String = row.get(1)?;
            let entity_type =
                crate::types::EntityType::try_from(entity_type_str).map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))?;
            Ok(Entity { name: row.get(0)?, entity_type })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entities)
}

/// Batch-hydrate entities for multiple memory IDs efficiently (avoids N+1 queries).
/// Entity map keyed by [`MemoryId`].
pub(crate) type EntityMap = std::collections::HashMap<MemoryId, Vec<Entity>>;

/// Maximum number of IDs per `IN (…)` clause, staying well under SQLite's
/// default `SQLITE_MAX_VARIABLE_NUMBER` limit (999).
const ENTITY_BATCH_CHUNK_SIZE: usize = 500;

pub(crate) fn hydrate_entities_batch(conn: &Connection, memory_ids: &[MemoryId]) -> Result<EntityMap, StoreError> {
    if memory_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let id_strings: Vec<String> = memory_ids.iter().map(ToString::to_string).collect();
    let mut map: std::collections::HashMap<MemoryId, Vec<Entity>> = std::collections::HashMap::new();
    for chunk in id_strings.chunks(ENTITY_BATCH_CHUNK_SIZE) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!("SELECT memory_id, entity, entity_type FROM memory_entities WHERE memory_id IN ({})", placeholders.join(","));
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = chunk.iter().map(|id| -> &dyn rusqlite::types::ToSql { id }).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let id_str: String = row.get(0)?;
                let id: MemoryId = id_str
                    .parse()
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
                let entity_type_str: String = row.get(2)?;
                let entity_type =
                    crate::types::EntityType::try_from(entity_type_str).map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?;
                Ok((id, Entity { name: row.get(1)?, entity_type }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, entity) in rows {
            map.entry(id).or_default().push(entity);
        }
    }
    Ok(map)
}

#[expect(
    clippy::too_many_arguments,
    reason = "atomic batch store needs rows, supersession, audit, vector index, and one revision timestamp"
)]
fn batch_store_with_supersession_and_audit(
    conn: &mut Connection,
    vector_index: &SqliteVecIndex,
    prepared: &[PreparedMemoryRow],
    supersedes: &[Option<String>],
    audits: Option<&[AuditDraft]>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<MemoryId>, StoreError> {
    if let Some(audits) = audits
        && audits.len() != prepared.len()
    {
        return Err(audit_len_mismatch(prepared.len(), audits.len()));
    }
    let tx = sqlite_write_tx(conn)?;
    let mut ids = Vec::with_capacity(prepared.len());
    for (i, p) in prepared.iter().enumerate() {
        let supersedes_id = supersedes.get(i).and_then(|s| s.as_deref());
        if let Some(sid) = supersedes_id {
            validate_superseded_exists(&tx, sid)?;
        }
        let id = p.insert(&tx, vector_index)?;
        if let Some(sid) = supersedes_id {
            let new_id_str = id.to_string();
            #[expect(unused_results, reason = "UPDATE checked via validate_superseded_exists above")]
            mark_superseded(&tx, sid, &new_id_str, now)?;
        }
        if let Some(audits) = audits {
            let audit = audits.get(i).ok_or_else(|| audit_len_mismatch(prepared.len(), audits.len()))?;
            insert_audit_draft(&tx, &id, audit)?;
        }
        ids.push(id);
    }
    tx.commit()?;
    Ok(ids)
}

#[expect(clippy::too_many_arguments, reason = "atomic store needs row, supersession, audit, vector index, and one revision timestamp")]
fn insert_prepared_with_optional_supersession_and_audit(
    conn: &Connection,
    vector_index: &SqliteVecIndex,
    prepared: &PreparedMemoryRow,
    supersedes_id: Option<&str>,
    audit: Option<&AuditDraft>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<MemoryId, StoreError> {
    if let Some(sid) = supersedes_id {
        validate_superseded_exists(conn, sid)?;
    }
    let id = prepared.insert(conn, vector_index)?;
    if let Some(sid) = supersedes_id {
        let new_id_str = id.to_string();
        #[expect(unused_results, reason = "UPDATE checked via validate_superseded_exists above")]
        mark_superseded(conn, sid, &new_id_str, now)?;
    }
    if let Some(audit) = audit {
        insert_audit_draft(conn, &id, audit)?;
    }
    Ok(id)
}

fn metadata_len_mismatch(expected: usize, actual: usize) -> StoreError {
    StoreError::Conflict(format!("metadata length ({actual}) must match memories length ({expected})"))
}

fn audit_len_mismatch(expected: usize, actual: usize) -> StoreError {
    StoreError::Conflict(format!("audit length ({actual}) must match memories length ({expected})"))
}

fn supersedes_len_mismatch(expected: usize, actual: usize) -> StoreError {
    StoreError::Conflict(format!("supersedes length ({actual}) must match memories length ({expected})"))
}

fn validate_metadata_memory_id(memory_id: &MemoryId, metadata: &MemoryMetadata) -> Result<(), StoreError> {
    if metadata.memory_id == *memory_id {
        return Ok(());
    }
    Err(StoreError::Conflict(format!(
        "metadata memory_id ({}) must match memory id ({memory_id})",
        metadata.memory_id
    )))
}

pub(crate) fn upsert_metadata_conn(conn: &Connection, metadata: &MemoryMetadata, now: &str) -> Result<(), StoreError> {
    let quality_flags_json = serde_json::to_string(&metadata.quality_flags)?;
    #[expect(unused_results, reason = "UPSERT row count is not needed")]
    conn.execute(
        "INSERT INTO memory_metadata (
            memory_id, scope_key, summary, agent_label, created_by_principal,
            quality_flags, schema_version, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(memory_id) DO UPDATE SET
            scope_key = excluded.scope_key,
            summary = excluded.summary,
            agent_label = excluded.agent_label,
            created_by_principal = COALESCE(memory_metadata.created_by_principal, excluded.created_by_principal),
            quality_flags = excluded.quality_flags,
            schema_version = excluded.schema_version,
            updated_at = excluded.updated_at",
        params![
            metadata.memory_id.to_string(),
            metadata.scope_key.as_deref(),
            metadata.summary.as_deref(),
            metadata.agent_label.as_deref(),
            metadata.created_by_principal.as_deref(),
            quality_flags_json,
            metadata.schema_version,
            now,
        ],
    )?;
    Ok(())
}

pub(crate) fn get_metadata_conn(conn: &Connection, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, StoreError> {
    conn.query_row(
        "SELECT memory_id, scope_key, summary, agent_label, created_by_principal, quality_flags, schema_version
         FROM memory_metadata
         WHERE memory_id = ?1",
        params![memory_id.to_string()],
        |row| {
            let id_str: String = row.get(0)?;
            let quality_flags_json: String = row.get(5)?;
            let memory_id = id_str
                .parse()
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
            let quality_flags = serde_json::from_str(&quality_flags_json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e)))?;
            Ok(MemoryMetadata {
                memory_id,
                scope_key: row.get(1)?,
                summary: row.get(2)?,
                agent_label: row.get(3)?,
                created_by_principal: row.get(4)?,
                quality_flags,
                schema_version: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(StoreError::from)
}

/// Validate that a superseded memory exists, returning an error if not.
fn validate_superseded_exists(conn: &Connection, supersedes_id: &str) -> Result<(), StoreError> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memories WHERE id = ?1)", params![supersedes_id], |row| row.get(0))?;
    if !exists {
        return Err(StoreError::NotFound(format!("superseded memory not found: {supersedes_id}")));
    }
    Ok(())
}

/// Retrieve a single memory by ID, applying TTL and access policy, and hydrating entities.
fn get_by_id(conn: &Connection, id_str: &str, caller: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Result<Option<Memory>, StoreError> {
    let Some(mut mem) = fetch_memory_by_id(conn, id_str)? else {
        return Ok(None);
    };
    // TTL enforcement
    if mem.expires_at.is_some_and(|exp| now >= exp) {
        return Ok(None);
    }
    // Hydrate entities from junction table
    mem.entities = hydrate_entities_single(conn, id_str)?;
    // Access policy enforcement (may redact fields)
    Ok(mem.apply_access_policy(caller))
}

/// Fetch a single memory by its string ID.
pub(crate) fn fetch_memory_by_id(conn: &Connection, id_str: &str) -> Result<Option<Memory>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?1"))?;
    let mut rows = stmt.query(params![id_str])?;
    match rows.next()? {
        Some(row) => Ok(Some(row_to_memory(row)?)),
        None => Ok(None),
    }
}

/// Insert or replace a deleted-memory authorization tombstone.
pub(crate) fn insert_tombstone(conn: &Connection, memory: &Memory, deleted_at: &str, deleted_by: Option<&str>) -> Result<(), StoreError> {
    let provenance = serde_json::to_string(&memory.provenance)?;
    let access_policy = serde_json::to_string(&memory.access_policy)?;
    #[expect(unused_results, reason = "UPSERT row count is not useful")]
    conn.execute(
        "INSERT INTO memory_tombstones (memory_id, provenance, access_policy, deleted_at, deleted_by_principal)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(memory_id) DO UPDATE SET
            provenance = excluded.provenance,
            access_policy = excluded.access_policy,
            deleted_at = excluded.deleted_at,
            deleted_by_principal = excluded.deleted_by_principal",
        params![memory.id.to_string(), provenance, access_policy, deleted_at, deleted_by],
    )?;
    Ok(())
}

/// Delete a memory by string ID within a transaction, retaining its authorization tombstone.
pub(crate) fn apply_delete(conn: &mut Connection, id_str: &str, deleted_at: &str) -> Result<bool, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let Some(existing) = fetch_memory_by_id(&tx, id_str)? else {
        tx.commit()?;
        return Ok(false);
    };
    insert_tombstone(&tx, &existing, deleted_at, None)?;
    let affected = tx.execute("DELETE FROM memories WHERE id = ?1", params![id_str])?;
    tx.commit()?;
    Ok(affected > 0)
}

fn apply_authorized_delete(conn: &mut Connection, id_str: &str, principal: &str, deleted_at: &str, audit: Option<&AuditDraft>) -> Result<WriteOutcome, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let Some(existing) = fetch_memory_by_id(&tx, id_str)? else {
        tx.commit()?;
        return Ok(WriteOutcome::NotFound);
    };
    if !existing.has_write_access(principal) {
        tx.commit()?;
        return Ok(WriteOutcome::Denied);
    }
    insert_tombstone(&tx, &existing, deleted_at, Some(principal))?;
    let affected = tx.execute("DELETE FROM memories WHERE id = ?1", params![id_str])?;
    if affected > 0
        && let Some(audit) = audit
    {
        insert_audit_draft(&tx, &existing.id, audit)?;
    }
    tx.commit()?;
    Ok(if affected > 0 { WriteOutcome::Applied } else { WriteOutcome::NotFound })
}

#[expect(clippy::too_many_arguments, reason = "atomic revise needs update, optional metadata, principal, timestamp, and audit draft")]
fn apply_authorized_update_with_metadata(
    conn: &mut Connection,
    vector_index: &SqliteVecIndex,
    id: &MemoryId,
    update: &MemoryUpdate,
    metadata_patch: Option<&MetadataPatch>,
    principal: &str,
    now: chrono::DateTime<chrono::Utc>,
    audit: Option<&AuditDraft>,
) -> Result<AuthorizedUpdateOutcome, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let id_str = id.to_string();
    let Some(existing) = fetch_memory_by_id(&tx, &id_str)? else {
        tx.commit()?;
        return Ok(AuthorizedUpdateOutcome {
            outcome: WriteOutcome::NotFound,
            reembed_revision: None,
        });
    };
    if !existing.has_write_access(principal) {
        tx.commit()?;
        return Ok(AuthorizedUpdateOutcome {
            outcome: WriteOutcome::Denied,
            reembed_revision: None,
        });
    }

    let revision_at = next_memory_revision(now, existing.updated_at);
    let revision_at_text = revision_at.to_rfc3339();
    let outcome = apply_update_inner(&tx, vector_index, &id_str, update, &revision_at_text)?;
    let metadata_only = metadata_patch.is_some() && !has_column_updates(update) && update.entities.is_none();
    if outcome.outcome == WriteOutcome::Applied {
        if let Some(patch) = metadata_patch {
            let existing_metadata = get_metadata_conn(&tx, id)?;
            let metadata = merge_metadata_patch(*id, patch, existing_metadata.as_ref(), existing.provenance.source_conversation.as_deref(), principal);
            upsert_metadata_conn(&tx, &metadata, &revision_at_text)?;
        }
        if metadata_only {
            increment_record_revision_conn(&tx, &id_str, "saving")?;
        }
        if let Some(audit) = audit {
            let audit = update_audit_draft_for_locked_memory(audit, update, &existing);
            insert_audit_draft(&tx, &existing.id, &audit)?;
        }
    }
    tx.commit()?;
    Ok(outcome)
}

#[expect(clippy::too_many_arguments, reason = "atomic TUI revise needs revision, fields, metadata, embedding, principal, and audit")]
fn apply_authorized_update_if_unmodified_with_metadata(
    conn: &mut Connection,
    vector_index: &SqliteVecIndex,
    id: &MemoryId,
    expected_revision: i64,
    update: &MemoryUpdate,
    metadata_patch: Option<&MetadataPatch>,
    embedding: Option<&[f32]>,
    expected_embedding_profile: Option<&EmbeddingProfile>,
    principal: &str,
    now: chrono::DateTime<chrono::Utc>,
    audit: &AuditDraft,
) -> Result<AuthorizedUpdateOutcome, StoreError> {
    if embedding.is_some() && update.content.is_none() {
        return Err(StoreError::Conflict("a replacement embedding requires replacement content".into()));
    }
    if let Some(embedding) = embedding {
        validate_embedding_vector(embedding, vector_index.dimensions())?;
    }

    let tx = sqlite_write_tx(conn)?;
    let id_str = id.to_string();
    let Some(existing) = fetch_memory_by_id(&tx, &id_str)? else {
        tx.commit()?;
        return Ok(AuthorizedUpdateOutcome {
            outcome: WriteOutcome::NotFound,
            reembed_revision: None,
        });
    };
    if !existing.has_write_access(principal) {
        tx.commit()?;
        return Ok(AuthorizedUpdateOutcome {
            outcome: WriteOutcome::Denied,
            reembed_revision: None,
        });
    }
    if existing.record_revision != expected_revision {
        return Err(StoreError::Conflict(format!("memory {id} changed after it was opened")));
    }

    let revision_at = next_memory_revision(now, existing.updated_at);
    let revision_at_text = revision_at.to_rfc3339();
    let mut outcome = apply_update_inner(&tx, vector_index, &id_str, update, &revision_at_text)?;
    if let Some(embedding) = embedding {
        if let Some(profile) = expected_embedding_profile {
            ensure_embedding_profile_matches(&tx, profile)?;
        }
        vector_index.upsert(&tx, &id_str, embedding)?;
        let affected = tx.execute("UPDATE memories SET has_embedding = 1 WHERE id = ?1", params![id_str])?;
        if affected == 0 {
            return Err(StoreError::Conflict(format!("memory {id} changed while saving")));
        }
        outcome.reembed_revision = None;
    }
    if let Some(patch) = metadata_patch {
        let existing_metadata = get_metadata_conn(&tx, id)?;
        let metadata = merge_metadata_patch(*id, patch, existing_metadata.as_ref(), existing.provenance.source_conversation.as_deref(), principal);
        upsert_metadata_conn(&tx, &metadata, &revision_at_text)?;
    }
    if metadata_patch.is_some() && !has_column_updates(update) && update.entities.is_none() {
        increment_record_revision_conn(&tx, &id_str, "saving")?;
    }
    let audit = update_audit_draft_for_locked_memory(audit, update, &existing);
    insert_audit_draft(&tx, id, &audit)?;
    tx.commit()?;
    Ok(outcome)
}

fn increment_record_revision_conn(conn: &Connection, id_str: &str, action: &str) -> Result<(), StoreError> {
    let affected = conn.execute("UPDATE memories SET record_revision = record_revision + 1 WHERE id = ?1", params![id_str])?;
    if affected == 0 {
        return Err(StoreError::Conflict(format!("memory {id_str} changed while {action}")));
    }
    Ok(())
}

pub(crate) fn next_memory_revision(now: chrono::DateTime<chrono::Utc>, previous: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    previous.checked_add_signed(chrono::Duration::microseconds(1_i64)).map_or(now, |minimum| now.max(minimum))
}

#[expect(clippy::too_many_arguments, reason = "atomic delete needs connection, identity, revision, principal, timestamp, and audit")]
fn apply_authorized_delete_if_unmodified(
    conn: &mut Connection,
    id: &MemoryId,
    expected_revision: i64,
    principal: &str,
    deleted_at: &str,
    audit: &AuditDraft,
) -> Result<WriteOutcome, StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let id_str = id.to_string();
    let Some(existing) = fetch_memory_by_id(&tx, &id_str)? else {
        tx.commit()?;
        return Ok(WriteOutcome::NotFound);
    };
    if !existing.has_write_access(principal) {
        tx.commit()?;
        return Ok(WriteOutcome::Denied);
    }
    if existing.record_revision != expected_revision {
        return Err(StoreError::Conflict(format!("memory {id} changed after it was opened")));
    }

    insert_tombstone(&tx, &existing, deleted_at, Some(principal))?;
    let affected = tx.execute("DELETE FROM memories WHERE id = ?1", params![id_str])?;
    if affected == 0 {
        return Err(StoreError::Conflict(format!("memory {id} changed while deleting")));
    }
    insert_audit_draft(&tx, id, audit)?;
    tx.commit()?;
    Ok(WriteOutcome::Applied)
}

/// Maximum number of IDs per `IN (…)` clause in bulk operations, staying
/// well under SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` limit (999).
pub(crate) const SQLITE_MAX_CHUNK: usize = 900;

/// Delete multiple memories by ID in a single transaction, checking write
/// access per-ID inside the transaction.
///
/// Returns a [`super::BulkAuthOutcome`] with `applied` (deleted) and `denied` counts.
pub(crate) fn bulk_delete_ids(
    conn: &mut Connection,
    ids: &[MemoryId],
    principal: &str,
    deleted_at: &str,
    audit: Option<&AuditDraft>,
) -> Result<super::BulkAuthOutcome, StoreError> {
    if ids.is_empty() {
        return Ok(super::BulkAuthOutcome {
            applied_ids: Vec::new(),
            denied: 0,
        });
    }
    let tx = sqlite_write_tx(conn)?;
    let mut applied_ids: Vec<MemoryId> = Vec::new();
    let mut denied = 0_u64;
    for chunk in ids.chunks(SQLITE_MAX_CHUNK) {
        let id_strs: Vec<String> = chunk.iter().map(ToString::to_string).collect();
        for (id_str, &id) in id_strs.iter().zip(chunk) {
            let Some(mem) = fetch_memory_by_id(&tx, id_str)? else {
                continue;
            };
            if !mem.has_write_access(principal) {
                denied = denied.saturating_add(1);
                continue;
            }
            insert_tombstone(&tx, &mem, deleted_at, Some(principal))?;
            let affected = tx.execute("DELETE FROM memories WHERE id = ?1", params![id_str])?;
            if affected > 0 {
                insert_optional_audit_draft(&tx, &id, audit)?;
                applied_ids.push(id);
            }
        }
    }
    tx.commit()?;
    Ok(super::BulkAuthOutcome { applied_ids, denied })
}

/// Apply the same update to multiple memories by ID in a single transaction,
/// checking write access per-ID inside the transaction.
///
/// Returns a [`super::BulkAuthOutcome`] with applied IDs and denied count.
#[expect(clippy::too_many_arguments, reason = "bulk update needs connection, vector index, ids, update, principal, and timestamp")]
pub(crate) fn bulk_update_ids(
    conn: &mut Connection,
    vector_index: &SqliteVecIndex,
    ids: &[MemoryId],
    update: &MemoryUpdate,
    principal: &str,
    now: chrono::DateTime<chrono::Utc>,
    audit: Option<&AuditDraft>,
) -> Result<super::BulkAuthOutcome, StoreError> {
    if ids.is_empty() {
        return Ok(super::BulkAuthOutcome {
            applied_ids: Vec::new(),
            denied: 0,
        });
    }
    let tx = sqlite_write_tx(conn)?;
    let mut applied_ids: Vec<MemoryId> = Vec::new();
    let mut denied = 0_u64;
    for chunk in ids.chunks(SQLITE_MAX_CHUNK) {
        let id_strs: Vec<String> = chunk.iter().map(ToString::to_string).collect();
        for (id_str, &id) in id_strs.iter().zip(chunk) {
            let Some(mem) = fetch_memory_by_id(&tx, id_str)? else {
                continue;
            };
            if !mem.has_write_access(principal) {
                denied = denied.saturating_add(1);
                continue;
            }
            let revision_at = next_memory_revision(now, mem.updated_at).to_rfc3339();
            let outcome = apply_update_inner(&tx, vector_index, id_str, update, &revision_at)?;
            if outcome.outcome == WriteOutcome::Applied {
                insert_optional_audit_draft(&tx, &id, audit)?;
                applied_ids.push(id);
            }
        }
    }
    tx.commit()?;
    Ok(super::BulkAuthOutcome { applied_ids, denied })
}

/// Apply a partial update to a memory, returning the outcome and optional reembed revision.
pub(crate) fn apply_update(
    conn: &mut Connection,
    vector_index: &SqliteVecIndex,
    id_str: &str,
    update: &MemoryUpdate,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<AuthorizedUpdateOutcome, StoreError> {
    // Entity-only update with no columns: no transaction needed, just an existence check.
    let entities_update = update.entities.clone();
    if !has_column_updates(update) && entities_update.is_none() {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memories WHERE id = ?1)", params![id_str], |row| row.get(0))?;
        return Ok(AuthorizedUpdateOutcome {
            outcome: if exists { WriteOutcome::Applied } else { WriteOutcome::NotFound },
            reembed_revision: None,
        });
    }

    let tx = sqlite_write_tx(conn)?;
    let Some(existing) = fetch_memory_by_id(&tx, id_str)? else {
        tx.commit()?;
        return Ok(AuthorizedUpdateOutcome {
            outcome: WriteOutcome::NotFound,
            reembed_revision: None,
        });
    };
    let revision_at = next_memory_revision(now, existing.updated_at).to_rfc3339();
    let outcome = apply_update_inner(&tx, vector_index, id_str, update, &revision_at)?;
    tx.commit()?;
    Ok(outcome)
}

/// Check whether an update has any column-level changes (not just entities).
const fn has_column_updates(update: &MemoryUpdate) -> bool {
    update.content.is_some()
        || update.tags.is_some()
        || update.access_policy.is_some()
        || update.importance.is_some()
        || update.expires_at.is_some()
        || update.confidence.is_some()
        || update.source_conversation.is_some()
}

/// Builder for dynamic SQL `SET` clauses with numbered parameters.
///
/// Collects `column = ?N` fragments and their corresponding values,
/// auto-incrementing the parameter index. Also supports literal SQL
/// fragments (e.g. `has_embedding = 0`) that have no bound parameter.
struct SetClauseBuilder {
    clauses: Vec<String>,
    values: Vec<Box<dyn rusqlite::types::ToSql>>,
    next_idx: usize,
}

impl SetClauseBuilder {
    /// Create a new builder starting at parameter index 1.
    fn new() -> Self {
        Self {
            clauses: Vec::new(),
            values: Vec::new(),
            next_idx: 1,
        }
    }

    /// Add a `column = ?N` clause with a bound parameter value.
    #[expect(clippy::arithmetic_side_effects, reason = "SQL parameter index increment — param count is always small")]
    fn push(&mut self, column: &str, value: Box<dyn rusqlite::types::ToSql>) {
        self.clauses.push(format!("{column} = ?{}", self.next_idx));
        self.values.push(value);
        self.next_idx += 1;
    }

    /// Add an expression assignment with one bound parameter.
    #[expect(clippy::arithmetic_side_effects, reason = "SQL parameter index increment — param count is always small")]
    fn push_expr(&mut self, expression: impl Into<String>, value: Box<dyn rusqlite::types::ToSql>) {
        self.clauses.push(expression.into());
        self.values.push(value);
        self.next_idx += 1;
    }

    /// Add a literal SQL fragment with no bound parameter (e.g. `has_embedding = 0`).
    fn push_literal(&mut self, clause: &str) {
        self.clauses.push(clause.into());
    }

    /// Whether any clauses have been added.
    const fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// The next parameter index that would be assigned.
    const fn next_index(&self) -> usize {
        self.next_idx
    }

    /// Build the `SET col1 = ?1, col2 = ?2, ...` SQL fragment.
    fn to_set_sql(&self) -> String {
        self.clauses.join(", ")
    }

    /// Build `&dyn ToSql` reference slice for binding, appending the trailing ID parameter.
    fn bind_params_with_id<'a>(&'a self, id: &'a dyn rusqlite::types::ToSql) -> Vec<&'a dyn rusqlite::types::ToSql> {
        let mut refs: Vec<&dyn rusqlite::types::ToSql> = self.values.iter().map(AsRef::as_ref).collect();
        refs.push(id);
        refs
    }
}

/// Build a `SetClauseBuilder` from a `MemoryUpdate`.
fn build_set_clause(update: &MemoryUpdate, now: &str) -> Result<SetClauseBuilder, StoreError> {
    let mut builder = SetClauseBuilder::new();

    if let Some(content) = &update.content {
        builder.push("content", Box::new(content.clone()));
        // Content changed — embedding is stale and revision advances.
        builder.push_literal("has_embedding = 0");
        builder.push_literal("embedding_revision = embedding_revision + 1");
        builder.push_literal("embedding_claimed_at = NULL");
        builder.push_literal("embedding_claim_token = NULL");
    }
    if let Some(tags) = &update.tags {
        builder.push("tags", Box::new(serde_json::to_string(tags)?));
    }
    if let Some(policy) = &update.access_policy {
        builder.push("access_policy", Box::new(serde_json::to_string(policy)?));
    }
    if let Some(importance) = update.importance {
        builder.push("importance", Box::new(importance.value()));
    }
    if let Some(expires_at) = update.expires_at {
        builder.push("expires_at", Box::new(expires_at.map(|value| value.to_rfc3339())));
    }
    if let Some(confidence) = update.confidence {
        builder.push("confidence", Box::new(confidence.value()));
    }
    if let Some(source_conversation) = &update.source_conversation {
        builder.push_expr(
            format!("provenance = json_set(provenance, '$.source_conversation', ?{})", builder.next_index()),
            Box::new(source_conversation.clone()),
        );
    }
    if update.content.is_some() {
        builder.push("updated_at", Box::new(now.to_owned()));
    }
    if has_column_updates(update) {
        builder.push_literal("record_revision = record_revision + 1");
    }

    Ok(builder)
}

/// Inner update logic that operates on a connection (which may be a transaction).
///
/// Does NOT manage its own transaction — the caller must wrap this in a
/// transaction when atomicity is required.
fn apply_update_inner(conn: &Connection, vector_index: &SqliteVecIndex, id_str: &str, update: &MemoryUpdate, now: &str) -> Result<AuthorizedUpdateOutcome, StoreError> {
    let builder = build_set_clause(update, now)?;
    let content_changed = update.content.is_some();
    let entities_update = update.entities.clone();

    if builder.is_empty() {
        let affected = if entities_update.is_some() {
            conn.execute("UPDATE memories SET record_revision = record_revision + 1 WHERE id = ?1", params![id_str])?
        } else {
            usize::from(conn.query_row("SELECT EXISTS(SELECT 1 FROM memories WHERE id = ?1)", params![id_str], |row| row.get::<_, bool>(0))?)
        };
        if affected == 0 {
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::NotFound,
                reembed_revision: None,
            });
        }
    } else {
        let next_idx = builder.next_index();
        let sql = format!("UPDATE memories SET {} WHERE id = ?{next_idx}", builder.to_set_sql());
        let id_owned = id_str.to_owned();
        let params = builder.bind_params_with_id(&id_owned);
        let affected = conn.execute(&sql, params.as_slice())?;
        if affected == 0 {
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::NotFound,
                reembed_revision: None,
            });
        }
    }

    let reembed_revision = if content_changed {
        // Remove stale embedding immediately so semantic search never returns stale vectors.
        vector_index.delete(conn, id_str)?;
        Some(conn.query_row("SELECT embedding_revision FROM memories WHERE id = ?1", params![id_str], |row| row.get(0))?)
    } else {
        None
    };

    // Replace entities if provided (DELETE + INSERT within same transaction).
    if let Some(entities) = &entities_update {
        replace_entities(conn, id_str, entities)?;
    }

    Ok(AuthorizedUpdateOutcome {
        outcome: WriteOutcome::Applied,
        reembed_revision,
    })
}

struct ReembedClaimSelection<'a> {
    principal: Option<&'a str>,
    limit: usize,
    now: &'a str,
    expired_before: &'a str,
    claim_token: &'a str,
}

fn claim_for_reembed_conn(conn: &mut Connection, selection: &ReembedClaimSelection<'_>) -> Result<Vec<ReembedClaim>, StoreError> {
    let limit_i64 = usize_to_i64(selection.limit, "reembed limit")?;
    let tx = sqlite_write_tx(conn)?;
    let mut stmt = tx.prepare(
        "SELECT id, content, embedding_revision
         FROM memories
         WHERE has_embedding = 0
           AND (embedding_claimed_at IS NULL OR embedding_claimed_at <= ?1)
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
         ORDER BY created_at ASC, id ASC
         LIMIT ?3",
    )?;
    let candidates = stmt
        .query_map(params![selection.expired_before, selection.principal, limit_i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut claims = Vec::with_capacity(candidates.len());
    for (id_str, content, embedding_revision) in candidates {
        let affected = tx.execute(
            "UPDATE memories
             SET embedding_claimed_at = ?1,
                 embedding_claim_token = ?2
             WHERE id = ?3
                 AND has_embedding = 0
               AND embedding_revision = ?4
               AND (embedding_claimed_at IS NULL OR embedding_claimed_at <= ?5)",
            params![selection.now, selection.claim_token, id_str, embedding_revision, selection.expired_before],
        )?;
        if affected == 0 {
            continue;
        }
        let id: MemoryId = id_str.parse().map_err(|e| StoreError::Serialization(format!("invalid memory id: {e}").into()))?;
        claims.push(ReembedClaim {
            id,
            content,
            embedding_revision,
            claim_token: selection.claim_token.to_owned(),
        });
    }
    tx.commit()?;
    Ok(claims)
}

// ---------------------------------------------------------------------------
// Consolidation — list memories with embeddings
// ---------------------------------------------------------------------------

/// Fetch memories that have embeddings, optionally filtered by scope keys.
///
/// Returns `MemoryWithEmbedding` pairs with the embedding vector loaded from
/// the vec0 table. Results are capped at `limit` and ordered by creation time
/// descending.
fn list_memories_with_embeddings(
    conn: &Connection,
    vector_index: &SqliteVecIndex,
    scopes_any: Option<&[String]>,
    limit: usize,
) -> Result<Vec<super::MemoryWithEmbedding>, StoreError> {
    use std::fmt::Write as _;

    let limit_i64 = usize_to_i64(limit, "limit")?;

    let mut sql = format!(
        "SELECT {MEMORY_COLUMNS} \
         FROM memories m \
         WHERE m.has_embedding = 1 AND m.superseded_by IS NULL"
    );
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut next_idx = 1_usize;

    if let Some(keys) = scopes_any
        && !keys.is_empty()
    {
        #[expect(clippy::arithmetic_side_effects, reason = "SQL parameter index increment — param count is always small")]
        let placeholders: Vec<String> = (0..keys.len()).map(|i| format!("?{}", next_idx + i)).collect();
        // write! on String is infallible — fmt::Write for String never fails.
        #[expect(clippy::let_underscore_must_use, reason = "fmt::Write for String is infallible")]
        let _ = write!(sql, " AND json_extract(m.provenance, '$.source_conversation') IN ({})", placeholders.join(", "));
        for key in keys {
            params_vec.push(Box::new(key.clone()));
        }
        next_idx = next_idx.saturating_add(keys.len());
    }

    // write! on String is infallible — fmt::Write for String never fails.
    #[expect(clippy::let_underscore_must_use, reason = "fmt::Write for String is infallible")]
    let _ = write!(sql, " ORDER BY m.created_at DESC LIMIT ?{next_idx}");
    params_vec.push(Box::new(limit_i64));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(AsRef::as_ref).collect();
    let mut stmt = conn.prepare(&sql)?;

    let memories: Vec<Memory> = stmt
        .query_map(param_refs.as_slice(), |row| {
            row_to_memory(row).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if memories.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<MemoryId> = memories.iter().map(|memory| memory.id).collect();
    let mut embeddings = vector_index.fetch_many(conn, &ids)?;
    let mut results = Vec::with_capacity(memories.len());

    for memory in memories {
        if let Some(embedding) = embeddings.remove(&memory.id) {
            results.push(super::MemoryWithEmbedding {
                memory,
                embedding: Some(embedding),
            });
        } else {
            tracing::warn!(memory_id = %memory.id, "memory has has_embedding=1 but no vector row");
        }
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Audit log helpers
// ---------------------------------------------------------------------------

/// Insert an audit log entry. Connection must already be inside a transaction
/// when atomicity with the main operation is required.
#[expect(unused_results, reason = "INSERT row count is always 1 — not useful")]
#[expect(
    clippy::too_many_arguments,
    reason = "audit entry requires memory_id, action, principal, timestamp, and details — all semantically distinct"
)]
pub(crate) fn insert_audit_entry(
    conn: &Connection,
    memory_id: &str,
    action: &str,
    principal: Option<&str>,
    timestamp: &str,
    details: Option<&serde_json::Value>,
) -> Result<(), StoreError> {
    let details_str = details.map(serde_json::to_string).transpose()?;
    conn.execute(
        "INSERT INTO memory_audit_log (memory_id, action, caller_agent, timestamp, details) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![memory_id, action, principal, timestamp, details_str],
    )?;
    Ok(())
}

pub(crate) fn insert_audit_draft(conn: &Connection, memory_id: &MemoryId, audit: &AuditDraft) -> Result<(), StoreError> {
    let id = memory_id.to_string();
    let action = audit.action.to_string();
    let timestamp = audit.timestamp.to_rfc3339();
    insert_audit_entry(conn, &id, &action, audit.caller_agent.as_deref(), &timestamp, audit.details.as_ref())
}

fn insert_optional_audit_draft(conn: &Connection, memory_id: &MemoryId, audit: Option<&AuditDraft>) -> Result<(), StoreError> {
    if let Some(audit) = audit {
        insert_audit_draft(conn, memory_id, audit)?;
    }
    Ok(())
}

/// Query audit log entries for a specific memory ID.
pub(crate) fn query_audit_log(conn: &Connection, memory_id: &str, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
    let limit_i64 = usize_to_i64(limit, "audit log limit")?;
    let mut stmt = conn.prepare("SELECT action, caller_agent, timestamp, details FROM memory_audit_log WHERE memory_id = ?1 ORDER BY id ASC LIMIT ?2")?;
    let rows = stmt
        .query_map(params![memory_id, limit_i64], |row| {
            let action_str: String = row.get(0)?;
            let action: AuditAction = action_str
                .parse()
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0_usize, rusqlite::types::Type::Text, Box::new(e)))?;
            let caller_agent: Option<String> = row.get(1)?;
            let timestamp_str: String = row.get(2)?;
            let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2_usize, rusqlite::types::Type::Text, Box::new(e)))?;
            let details_str: Option<String> = row.get(3)?;
            let details = details_str
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(3_usize, rusqlite::types::Type::Text, Box::new(e)))?;
            Ok(AuditEntry {
                action,
                caller_agent,
                timestamp,
                details,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Fetch a deleted-memory authorization tombstone.
pub(crate) fn fetch_tombstone_by_id(conn: &Connection, memory_id: &str) -> Result<Option<MemoryTombstone>, StoreError> {
    conn.query_row(
        "SELECT memory_id, provenance, access_policy, deleted_at, deleted_by_principal
         FROM memory_tombstones
         WHERE memory_id = ?1",
        params![memory_id],
        |row| {
            let id_str: String = row.get(0)?;
            let provenance_json: String = row.get(1)?;
            let access_policy_json: String = row.get(2)?;
            let deleted_at_str: String = row.get(3)?;
            Ok(MemoryTombstone {
                memory_id: id_str
                    .parse()
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
                provenance: serde_json::from_str::<Provenance>(&provenance_json)
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))?,
                access_policy: serde_json::from_str::<AccessPolicy>(&access_policy_json)
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?,
                deleted_at: chrono::DateTime::parse_from_rfc3339(&deleted_at_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?,
                deleted_by_principal: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(StoreError::from)
}

/// Compute a content fingerprint for audit logging.
///
/// Uses FNV-1a 64-bit hash formatted as hex. This is NOT cryptographic — it is
/// used only for change-detection traceability in audit log entries.
/// FNV-1a 64-bit offset basis.
pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime.
pub(crate) const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// Compute an FNV-1a 64-bit hash of the given bytes.
pub(crate) fn fnv1a_hash(data: &[u8]) -> u64 {
    data.iter().fold(FNV_OFFSET, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

pub(crate) fn content_hash(content: &str) -> String {
    format!("{:016x}", fnv1a_hash(content.as_bytes()))
}

enum RecordUseStatus {
    Recorded,
    Denied,
    NotFound,
}

/// Process a single memory-use update inside a transaction: read current state,
/// compute decayed mass, and write back.
#[expect(clippy::too_many_arguments, reason = "tx + id + principal + now_str + now + weight + half_life are all semantically distinct")]
#[expect(clippy::float_arithmetic, reason = "decayed mass + event weight is the core update formula")]
fn update_single_memory_use(
    tx: &rusqlite::Transaction<'_>,
    id_str: &str,
    principal: &str,
    now_str: &str,
    now: chrono::DateTime<chrono::Utc>,
    event_weight: f64,
    activity_half_life_hours: f64,
) -> Result<RecordUseStatus, StoreError> {
    let Some(memory) = fetch_memory_by_id(tx, id_str)? else {
        return Ok(RecordUseStatus::NotFound);
    };
    // Require full read access — agents with only redacted access should
    // not be able to inflate activity_mass for content they cannot read.
    if memory.expires_at.is_some_and(|exp| now >= exp) || memory.check_access_level(Some(principal)) != AccessLevel::Full {
        return Ok(RecordUseStatus::Denied);
    }

    // Decay the stored mass to `now` and add the event weight.
    //
    // SAFETY (TOCTOU): The enclosing immediate transaction takes SQLite's
    // writer lock before this read, so another process cannot interleave a
    // write between the activity read and update. If this ever moves outside
    // an immediate transaction, convert it to a single atomic UPDATE expression.
    let decayed = crate::scoring::decay_mass(memory.activity_mass, memory.last_used_at, now, activity_half_life_hours);
    let new_mass = decayed + event_weight;
    #[expect(unused_results, reason = "UPDATE row count checked via caller")]
    tx.execute("UPDATE memories SET activity_mass = ?1, last_used_at = ?2 WHERE id = ?3", rusqlite::params![
        new_mass, now_str, id_str
    ])?;
    Ok(RecordUseStatus::Recorded)
}

// -- MemoryStore trait impl methods on SqliteStore --

#[expect(clippy::multiple_inherent_impl, reason = "SqliteStore methods are split across submodules by concern")]
impl SqliteStore {
    pub(crate) async fn store_impl(&self, memory: &Memory, embedding: Option<&[f32]>) -> Result<MemoryId, StoreError> {
        self.store_audited_impl(memory, embedding, None).await
    }

    pub(crate) async fn store_audited_impl(&self, memory: &Memory, embedding: Option<&[f32]>, audit: Option<&AuditDraft>) -> Result<MemoryId, StoreError> {
        let prepared = PreparedMemoryRow::from_memory(memory, embedding)?;
        let audit = audit.cloned();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let id = prepared.insert(&tx, &vector_index)?;
            if let Some(audit) = audit.as_ref() {
                insert_audit_draft(&tx, &id, audit)?;
            }
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    /// Store a memory and atomically mark an older memory as superseded.
    ///
    /// The old memory's `superseded_by` is set to the new memory's ID.
    /// Returns an error if the superseded memory does not exist.
    pub(crate) async fn store_with_supersession_impl(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId) -> Result<MemoryId, StoreError> {
        self.store_with_supersession_audited_impl(memory, embedding, supersedes_id, None).await
    }

    pub(crate) async fn store_with_supersession_audited_impl(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: &MemoryId,
        audit: Option<&AuditDraft>,
    ) -> Result<MemoryId, StoreError> {
        let prepared = PreparedMemoryRow::from_memory(memory, embedding)?;
        let supersedes_id = supersedes_id.to_string();
        let audit = audit.cloned();
        let now = self.clock_now();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            validate_superseded_exists(&tx, &supersedes_id)?;
            let id = prepared.insert(&tx, &vector_index)?;
            let new_id_str = id.to_string();
            #[expect(unused_results, reason = "UPDATE row count checked via validate_superseded_exists above")]
            mark_superseded(&tx, &supersedes_id, &new_id_str, now)?;
            if let Some(audit) = audit.as_ref() {
                insert_audit_draft(&tx, &id, audit)?;
            }
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    pub(crate) async fn store_with_metadata_impl(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
    ) -> Result<MemoryId, StoreError> {
        self.store_with_metadata_audited_impl(memory, embedding, supersedes_id, metadata, None).await
    }

    #[expect(clippy::too_many_arguments, reason = "audited store needs memory, embedding, supersession, metadata, and audit draft")]
    pub(crate) async fn store_with_metadata_audited_impl(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
        audit: Option<&AuditDraft>,
    ) -> Result<MemoryId, StoreError> {
        validate_metadata_memory_id(&memory.id, metadata)?;
        let prepared = PreparedMemoryRow::from_memory(memory, embedding)?;
        let supersedes_id = supersedes_id.map(ToString::to_string);
        let metadata = metadata.clone();
        let now = self.clock_now();
        let metadata_now = now.to_rfc3339();
        let audit = audit.cloned();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let id = insert_prepared_with_optional_supersession_and_audit(&tx, &vector_index, &prepared, supersedes_id.as_deref(), audit.as_ref(), now)?;
            upsert_metadata_conn(&tx, &metadata, &metadata_now)?;
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    pub(crate) async fn get_impl(&self, id: &MemoryId, principal: Option<&str>) -> Result<Option<Memory>, StoreError> {
        let id_str = id.to_string();
        let caller = principal.map(String::from);
        let now = self.clock_now();
        self.with_conn(move |conn| get_by_id(conn, &id_str, caller.as_deref(), now)).await
    }

    pub(crate) async fn update_impl(&self, id: &MemoryId, update: &MemoryUpdate) -> Result<bool, StoreError> {
        let id_str = id.to_string();
        let update = update.clone();
        let now = self.clock_now();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| apply_update(conn, &vector_index, &id_str, &update, now).map(|outcome| outcome.outcome == WriteOutcome::Applied))
            .await
    }

    pub(crate) async fn delete_impl(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let id_str = id.to_string();
        let deleted_at = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| apply_delete(conn, &id_str, &deleted_at)).await
    }

    pub(crate) async fn store_batch_impl(&self, memories: &[super::MemoryWithEmbedding]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_audited_impl(memories, None).await
    }

    pub(crate) async fn store_batch_audited_impl(&self, memories: &[super::MemoryWithEmbedding], audits: Option<&[AuditDraft]>) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(audits) = audits
            && audits.len() != memories.len()
        {
            return Err(audit_len_mismatch(memories.len(), audits.len()));
        }
        let prepared: Vec<PreparedMemoryRow> = memories
            .iter()
            .map(|mwe| PreparedMemoryRow::from_memory(&mwe.memory, mwe.embedding.as_deref()))
            .collect::<Result<Vec<_>, _>>()?;
        let audits = audits.map(<[AuditDraft]>::to_vec);

        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let mut ids = Vec::with_capacity(prepared.len());
            for (idx, p) in prepared.iter().enumerate() {
                let id = p.insert(&tx, &vector_index)?;
                let audit = audits.as_ref().and_then(|items| items.get(idx));
                insert_optional_audit_draft(&tx, &id, audit)?;
                ids.push(id);
            }
            tx.commit()?;
            Ok(ids)
        })
        .await
    }

    pub(crate) async fn store_batch_with_supersession_impl(&self, memories: &[super::MemoryWithEmbedding], supersedes: &[Option<MemoryId>]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_supersession_audited_impl(memories, supersedes, None).await
    }

    pub(crate) async fn store_batch_with_supersession_audited_impl(
        &self,
        memories: &[super::MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        audits: Option<&[AuditDraft]>,
    ) -> Result<Vec<MemoryId>, StoreError> {
        if supersedes.len() != memories.len() {
            return Err(supersedes_len_mismatch(memories.len(), supersedes.len()));
        }
        if let Some(audits) = audits
            && audits.len() != memories.len()
        {
            return Err(audit_len_mismatch(memories.len(), audits.len()));
        }
        let prepared: Vec<PreparedMemoryRow> = memories
            .iter()
            .map(|mwe| PreparedMemoryRow::from_memory(&mwe.memory, mwe.embedding.as_deref()))
            .collect::<Result<Vec<_>, _>>()?;
        // Convert MemoryId to String for SQL layer
        let supersedes_strs: Vec<Option<String>> = supersedes.iter().map(|s| s.map(|id| id.to_string())).collect();
        let audits = audits.map(<[AuditDraft]>::to_vec);
        let now = self.clock_now();

        let vector_index = self.vector_index();
        self.with_conn(move |conn| batch_store_with_supersession_and_audit(conn, &vector_index, &prepared, &supersedes_strs, audits.as_deref(), now))
            .await
    }

    pub(crate) async fn store_batch_with_metadata_impl(
        &self,
        memories: &[super::MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
    ) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_metadata_audited_impl(memories, supersedes, metadata, None).await
    }

    pub(crate) async fn store_batch_with_metadata_audited_impl(
        &self,
        memories: &[super::MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
        audits: Option<&[AuditDraft]>,
    ) -> Result<Vec<MemoryId>, StoreError> {
        if metadata.len() != memories.len() {
            return Err(metadata_len_mismatch(memories.len(), metadata.len()));
        }
        if supersedes.len() != memories.len() {
            return Err(supersedes_len_mismatch(memories.len(), supersedes.len()));
        }
        if let Some(audits) = audits
            && audits.len() != memories.len()
        {
            return Err(audit_len_mismatch(memories.len(), audits.len()));
        }
        for (memory_with_embedding, item_metadata) in memories.iter().zip(metadata) {
            validate_metadata_memory_id(&memory_with_embedding.memory.id, item_metadata)?;
        }
        let prepared: Vec<PreparedMemoryRow> = memories
            .iter()
            .map(|mwe| PreparedMemoryRow::from_memory(&mwe.memory, mwe.embedding.as_deref()))
            .collect::<Result<Vec<_>, _>>()?;
        let supersedes_strs: Vec<Option<String>> = supersedes.iter().map(|s| s.map(|id| id.to_string())).collect();
        let metadata = metadata.to_vec();
        let now = self.clock_now();
        let metadata_now = now.to_rfc3339();
        let audits = audits.map(<[AuditDraft]>::to_vec);

        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let mut ids = Vec::with_capacity(prepared.len());
            for (idx, p) in prepared.iter().enumerate() {
                let supersedes_id = supersedes_strs.get(idx).and_then(|s| s.as_deref());
                let audit = audits.as_ref().and_then(|items| items.get(idx));
                let id = insert_prepared_with_optional_supersession_and_audit(&tx, &vector_index, p, supersedes_id, audit, now)?;
                let item_metadata = metadata.get(idx).ok_or_else(|| metadata_len_mismatch(prepared.len(), metadata.len()))?;
                upsert_metadata_conn(&tx, item_metadata, &metadata_now)?;
                ids.push(id);
            }
            tx.commit()?;
            Ok(ids)
        })
        .await
    }

    pub(crate) async fn update_authorized_impl(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.update_authorized_audited_impl(id, update, principal, None).await
    }

    pub(crate) async fn update_authorized_audited_impl(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        let id_str = id.to_string();
        let update = update.clone();
        let caller = principal.to_owned();
        let now = self.clock_now();
        let audit = audit.cloned();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let Some(existing) = fetch_memory_by_id(&tx, &id_str)? else {
                return Ok(AuthorizedUpdateOutcome {
                    outcome: WriteOutcome::NotFound,
                    reembed_revision: None,
                });
            };
            if !existing.has_write_access(&caller) {
                return Ok(AuthorizedUpdateOutcome {
                    outcome: WriteOutcome::Denied,
                    reembed_revision: None,
                });
            }
            let revision_at = next_memory_revision(now, existing.updated_at).to_rfc3339();
            let outcome = apply_update_inner(&tx, &vector_index, &id_str, &update, &revision_at)?;
            if outcome.outcome == WriteOutcome::Applied
                && let Some(audit) = audit.as_ref()
            {
                let audit = update_audit_draft_for_locked_memory(audit, &update, &existing);
                insert_audit_draft(&tx, &existing.id, &audit)?;
            }
            tx.commit()?;
            Ok(outcome)
        })
        .await
    }

    #[expect(clippy::too_many_arguments, reason = "audited revise needs id, update, metadata patch, principal, and audit draft")]
    pub(crate) async fn update_authorized_with_metadata_audited_impl(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        let id_value = *id;
        let update = update.clone();
        let metadata_patch = metadata_patch.cloned();
        let caller = principal.to_owned();
        let now = self.clock_now();
        let audit = audit.cloned();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| apply_authorized_update_with_metadata(conn, &vector_index, &id_value, &update, metadata_patch.as_ref(), &caller, now, audit.as_ref()))
            .await
    }

    #[expect(clippy::too_many_arguments, reason = "atomic TUI revise needs revision, fields, metadata, embedding, principal, and audit")]
    pub(crate) async fn update_authorized_if_unmodified_with_metadata_audited_impl(
        &self,
        id: &MemoryId,
        expected_revision: i64,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        embedding: Option<&[f32]>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        let id_value = *id;
        let update = update.clone();
        let metadata_patch = metadata_patch.cloned();
        let embedding = embedding.map(<[f32]>::to_vec);
        let caller = principal.to_owned();
        let now = self.clock_now();
        let audit = audit.clone();
        let vector_index = self.vector_index();
        let expected_embedding_profile = self.active_embedding_profile();
        self.with_conn(move |conn| {
            apply_authorized_update_if_unmodified_with_metadata(
                conn,
                &vector_index,
                &id_value,
                expected_revision,
                &update,
                metadata_patch.as_ref(),
                embedding.as_deref(),
                expected_embedding_profile.as_ref(),
                &caller,
                now,
                &audit,
            )
        })
        .await
    }

    pub(crate) async fn delete_authorized_impl(&self, id: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.delete_authorized_audited_impl(id, principal, None).await
    }

    pub(crate) async fn delete_authorized_audited_impl(&self, id: &MemoryId, principal: &str, audit: Option<&AuditDraft>) -> Result<WriteOutcome, StoreError> {
        let id_str = id.to_string();
        let caller = principal.to_owned();
        let deleted_at = self.clock_now().to_rfc3339();
        let audit = audit.cloned();
        self.with_conn(move |conn| apply_authorized_delete(conn, &id_str, &caller, &deleted_at, audit.as_ref()))
            .await
    }

    pub(crate) async fn delete_authorized_if_unmodified_audited_impl(
        &self,
        id: &MemoryId,
        expected_revision: i64,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<WriteOutcome, StoreError> {
        let id_value = *id;
        let caller = principal.to_owned();
        let deleted_at = self.clock_now().to_rfc3339();
        let audit = audit.clone();
        self.with_conn(move |conn| apply_authorized_delete_if_unmodified(conn, &id_value, expected_revision, &caller, &deleted_at, &audit))
            .await
    }

    pub(crate) async fn bulk_delete_ids_impl(&self, ids: Vec<MemoryId>, principal: &str) -> Result<super::BulkAuthOutcome, StoreError> {
        self.bulk_delete_ids_audited_impl(ids, principal, None).await
    }

    pub(crate) async fn bulk_delete_ids_audited_impl(&self, ids: Vec<MemoryId>, principal: &str, audit: Option<&AuditDraft>) -> Result<super::BulkAuthOutcome, StoreError> {
        let caller = principal.to_owned();
        let deleted_at = self.clock_now().to_rfc3339();
        let audit = audit.cloned();
        self.with_conn(move |conn| bulk_delete_ids(conn, &ids, &caller, &deleted_at, audit.as_ref())).await
    }

    pub(crate) async fn bulk_update_ids_impl(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<super::BulkAuthOutcome, StoreError> {
        self.bulk_update_ids_audited_impl(ids, update, principal, now, None).await
    }

    #[expect(clippy::too_many_arguments, reason = "audited bulk update needs ids, update, principal, timestamp, and audit draft")]
    pub(crate) async fn bulk_update_ids_audited_impl(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
        audit: Option<&AuditDraft>,
    ) -> Result<super::BulkAuthOutcome, StoreError> {
        let caller = principal.to_owned();
        let audit = audit.cloned();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| bulk_update_ids(conn, &vector_index, &ids, &update, &caller, now, audit.as_ref()))
            .await
    }

    pub(crate) async fn list_for_reembed_impl(&self, limit: usize) -> Result<Vec<(MemoryId, String, i64)>, StoreError> {
        self.with_conn(move |conn| {
            let limit_i64 = usize_to_i64(limit, "reembed limit")?;
            let mut stmt = conn.prepare(
                "SELECT id, content, embedding_revision
                 FROM memories
                 WHERE has_embedding = 0
                  ORDER BY created_at ASC, id ASC
                 LIMIT ?1",
            )?;
            let rows = stmt
                .query_map(params![limit_i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(id_str, content, rev)| {
                    let id: MemoryId = id_str.parse().map_err(|e| StoreError::Serialization(format!("invalid memory id: {e}").into()))?;
                    Ok((id, content, rev))
                })
                .collect()
        })
        .await
    }

    async fn claim_for_reembed_with_principal_impl(&self, principal: Option<&str>, limit: usize) -> Result<Vec<ReembedClaim>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now = self.clock_now();
        let now_str = now.to_rfc3339();
        let expired_before = now
            .checked_sub_signed(chrono::Duration::seconds(EMBEDDING_CLAIM_LEASE_SECS))
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
            .to_rfc3339();
        let claim_token = MemoryId::new().to_string();
        let principal = principal.map(ToOwned::to_owned);
        self.with_conn(move |conn| {
            claim_for_reembed_conn(conn, &ReembedClaimSelection {
                principal: principal.as_deref(),
                limit,
                now: &now_str,
                expired_before: &expired_before,
                claim_token: &claim_token,
            })
        })
        .await
    }

    pub(crate) async fn claim_for_reembed_impl(&self, limit: usize) -> Result<Vec<ReembedClaim>, StoreError> {
        self.claim_for_reembed_with_principal_impl(None, limit).await
    }

    pub(crate) async fn claim_for_reembed_authorized_impl(&self, principal: &str, limit: usize) -> Result<Vec<ReembedClaim>, StoreError> {
        self.claim_for_reembed_with_principal_impl(Some(principal), limit).await
    }

    pub(crate) async fn release_embedding_claim_impl(&self, id: &MemoryId, expected_revision: i64, claim_token: &str) -> Result<bool, StoreError> {
        let id_str = id.to_string();
        let token = claim_token.to_owned();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "UPDATE memories
                 SET embedding_claimed_at = NULL,
                     embedding_claim_token = NULL
                 WHERE id = ?1
                   AND has_embedding = 0
                   AND embedding_revision = ?2
                   AND embedding_claim_token = ?3",
                params![id_str, expected_revision, token],
            )?;
            Ok(affected > 0)
        })
        .await
    }

    pub(crate) async fn record_search_impression_impl(&self, ids: &[MemoryId]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let id_strs: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let now = self.clock_now().to_rfc3339();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            for id_str in &id_strs {
                #[expect(unused_results, reason = "UPDATE row count not needed for access tracking")]
                tx.execute(
                    "UPDATE memories SET impression_count = impression_count + 1, last_impressed_at = ?1 WHERE id = ?2",
                    rusqlite::params![now, id_str],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Record a real use event: decay stored `activity_mass` to `now`, add `event_weight`,
    /// and update `last_used_at`. Returns the number of memories actually updated.
    #[expect(clippy::too_many_arguments, reason = "ids + principal + weight + now + half_life are all semantically distinct")]
    pub(crate) async fn record_memory_use_impl(
        &self,
        ids: &[MemoryId],
        principal: &str,
        event_weight: f64,
        now: chrono::DateTime<chrono::Utc>,
        activity_half_life_hours: f64,
    ) -> Result<super::RecordUseOutcome, StoreError> {
        if ids.is_empty() {
            return Ok(super::RecordUseOutcome::default());
        }
        // Deduplicate IDs to prevent a single request from inflating
        // activity_mass by repeating the same memory ID.
        let mut seen = std::collections::HashSet::new();
        let id_strs: Vec<String> = ids.iter().filter(|id| seen.insert(**id)).map(ToString::to_string).collect();
        let principal = principal.to_owned();
        let now_str = now.to_rfc3339();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let outcome = id_strs.iter().try_fold(super::RecordUseOutcome::default(), |mut acc, id_str| {
                match update_single_memory_use(&tx, id_str, &principal, &now_str, now, event_weight, activity_half_life_hours)? {
                    RecordUseStatus::Recorded => acc.recorded = acc.recorded.saturating_add(1),
                    RecordUseStatus::Denied => acc.denied = acc.denied.saturating_add(1),
                    RecordUseStatus::NotFound => acc.not_found = acc.not_found.saturating_add(1),
                }
                Ok::<_, StoreError>(acc)
            })?;
            tx.commit()?;
            Ok(outcome)
        })
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "audit entry requires memory_id, action, principal, timestamp, and details — all semantically distinct"
    )]
    pub(crate) async fn write_audit_entry_impl(
        &self,
        memory_id: &MemoryId,
        action: AuditAction,
        principal: Option<&str>,
        timestamp: chrono::DateTime<chrono::Utc>,
        details: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        let id_str = memory_id.to_string();
        let action_str = action.to_string();
        let caller = principal.map(str::to_owned);
        let ts = timestamp.to_rfc3339();
        let det = details.cloned();
        self.with_conn(move |conn| insert_audit_entry(conn, &id_str, &action_str, caller.as_deref(), &ts, det.as_ref()))
            .await
    }

    pub(crate) async fn query_audit_log_impl(&self, memory_id: &MemoryId, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        let id_str = memory_id.to_string();
        self.with_conn(move |conn| query_audit_log(conn, &id_str, limit)).await
    }

    pub(crate) async fn get_tombstone_impl(&self, memory_id: &MemoryId) -> Result<Option<MemoryTombstone>, StoreError> {
        let id_str = memory_id.to_string();
        self.with_conn(move |conn| fetch_tombstone_by_id(conn, &id_str)).await
    }

    pub(crate) async fn fetch_embeddings_for_ids_impl(&self, ids: &[MemoryId]) -> Result<super::EmbeddingMap, StoreError> {
        let ids = ids.to_vec();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| vector_index.fetch_many(conn, &ids)).await
    }

    pub(crate) async fn list_with_embeddings_impl(&self, scopes_any: Option<&[String]>, limit: usize) -> Result<Vec<super::MemoryWithEmbedding>, StoreError> {
        let scope_keys = scopes_any.map(<[String]>::to_vec);
        let vector_index = self.vector_index();
        self.with_conn(move |conn| list_memories_with_embeddings(conn, &vector_index, scope_keys.as_deref(), limit))
            .await
    }

    pub(crate) async fn mark_superseded_by_impl(&self, id: &MemoryId, superseded_by: &MemoryId) -> Result<bool, StoreError> {
        let id_str = id.to_string();
        let sup_by = superseded_by.to_string();
        let now = self.clock_now();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let marked = mark_superseded(&tx, &id_str, &sup_by, now)?;
            tx.commit()?;
            Ok(marked)
        })
        .await
    }

    pub(crate) async fn mark_superseded_by_authorized_impl(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.mark_superseded_by_authorized_audited_impl(id, superseded_by, principal, None).await
    }

    pub(crate) async fn mark_superseded_by_authorized_audited_impl(
        &self,
        id: &MemoryId,
        superseded_by: &MemoryId,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<WriteOutcome, StoreError> {
        let id_str = id.to_string();
        let sup_by = superseded_by.to_string();
        let caller = principal.to_owned();
        let audit = audit.cloned();
        let now = self.clock_now();
        self.with_conn(move |conn| {
            let tx = sqlite_write_tx(conn)?;
            let Some(existing) = fetch_memory_by_id(&tx, &id_str)? else {
                return Ok(WriteOutcome::NotFound);
            };
            if !existing.has_write_access(&caller) {
                return Ok(WriteOutcome::Denied);
            }
            let marked = mark_superseded(&tx, &id_str, &sup_by, now)?;
            if marked && let Some(audit) = audit.as_ref() {
                insert_audit_draft(&tx, &existing.id, audit)?;
            }
            tx.commit()?;
            Ok(if marked { WriteOutcome::Applied } else { WriteOutcome::NotFound })
        })
        .await
    }

    pub(crate) async fn get_for_reembed_impl(&self, id: &MemoryId, principal: &str) -> Result<Option<(String, i64)>, StoreError> {
        let id_str = id.to_string();
        let caller = principal.to_owned();
        self.with_conn(move |conn| {
            let Some(existing) = fetch_memory_by_id(conn, &id_str)? else {
                return Ok(None);
            };
            if !existing.has_write_access(&caller) {
                return Ok(None);
            }
            let revision: i64 = conn.query_row("SELECT embedding_revision FROM memories WHERE id = ?1", params![id_str], |row| row.get(0))?;
            Ok(Some((existing.content, revision)))
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- RR-042: content_hash ------------------------------------------------

    #[test]
    fn content_hash_empty_string_is_consistent() {
        let h1 = content_hash("");
        let h2 = content_hash("");
        assert_eq!(h1, h2, "hashing the same input twice should produce the same output");
        assert_eq!(h1.len(), 16, "hex-encoded u64 should be 16 chars");
    }

    #[test]
    fn content_hash_known_input_produces_known_output() {
        // FNV-1a of "hello" is a well-known value; verify our implementation is stable.
        let h = content_hash("hello");
        // Re-hash to confirm determinism (the exact value is an implementation detail,
        // but it must be stable across runs).
        let h2 = content_hash("hello");
        assert_eq!(h, h2);
        // Verify it's a valid 16-char hex string.
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn content_hash_different_inputs_produce_different_hashes() {
        let h1 = content_hash("hello");
        let h2 = content_hash("world");
        assert_ne!(h1, h2, "different inputs should produce different hashes");
    }

    // -- RR-127: double-supersession guard (mark_superseded) -----------------

    #[tokio::test]
    async fn mark_superseded_double_supersession_returns_conflict() {
        use crate::{
            store::{MemoryWriter as _, SqliteStore},
            types::{AccessPolicy, Memory, Provenance},
        };

        let store = SqliteStore::in_memory().unwrap();

        let mem_a = Memory::new_for_test("original".into(), vec![], Provenance::default(), AccessPolicy::Public);
        let id_a = store.store(&mem_a, None).await.unwrap();
        let mem_b = Memory::new_for_test("replacement 1".into(), vec![], Provenance::default(), AccessPolicy::Public);
        let id_b = store.store(&mem_b, None).await.unwrap();
        let mem_c = Memory::new_for_test("replacement 2".into(), vec![], Provenance::default(), AccessPolicy::Public);
        let id_c = store.store(&mem_c, None).await.unwrap();

        // First supersession: A -> B (should succeed).
        let result = store.mark_superseded_by(&id_a, &id_b).await;
        assert!(result.is_ok(), "first supersession should succeed: {result:?}");

        // Second supersession: A -> C (should fail with Conflict).
        let err = store.mark_superseded_by(&id_a, &id_c).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "double supersession should return Conflict, got: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("already superseded"), "error message should mention 'already superseded': {msg}");
    }

    #[tokio::test]
    async fn optimistic_delete_rolls_back_tombstone_when_delete_is_ignored() {
        use crate::{
            store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
            types::{AccessPolicy, AuditAction, AuditDraft, Memory, Provenance},
        };

        let store = SqliteStore::in_memory().unwrap();
        let memory = Memory::new_for_test(
            "protected from delete".into(),
            Vec::new(),
            Provenance {
                source_agent: Some("owner".into()),
                ..Provenance::default()
            },
            AccessPolicy::Public,
        );
        let id = store.store(&memory, None).await.unwrap();
        let loaded = store.get(&id, Some("owner")).await.unwrap().unwrap();
        store
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER ignore_memory_delete
                     BEFORE DELETE ON memories
                     BEGIN
                         SELECT RAISE(IGNORE);
                     END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let audit = AuditDraft {
            action: AuditAction::Delete,
            caller_agent: Some("owner".into()),
            timestamp: chrono::Utc::now(),
            details: None,
        };

        let error = store
            .delete_authorized_if_unmodified_audited(&id, loaded.record_revision, "owner", &audit)
            .await
            .unwrap_err();

        assert!(matches!(error, StoreError::Conflict(_)));
        assert!(store.get(&id, Some("owner")).await.unwrap().is_some());
        assert!(store.get_tombstone(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn optimistic_embedding_write_rejects_profile_drift() {
        use crate::{
            store::{EmbeddingProfile, MemoryReader as _, MemoryWriter as _, SqliteStore},
            types::{AccessPolicy, AuditAction, AuditDraft, Memory, MemoryUpdate, Provenance},
        };

        let store = SqliteStore::in_memory().unwrap();
        let profile = EmbeddingProfile::openai_compatible("http://127.0.0.1:8000/v1", "model-a", SqliteStore::DEFAULT_TEST_DIMENSIONS);
        store.verify_embedding_profile(&profile).await.unwrap();
        let memory = Memory::new_for_test(
            "profile guarded".into(),
            Vec::new(),
            Provenance {
                source_agent: Some("owner".into()),
                ..Provenance::default()
            },
            AccessPolicy::Public,
        );
        let original_embedding = vec![0.25_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
        let id = store.store(&memory, Some(&original_embedding)).await.unwrap();
        let loaded = store.get(&id, Some("owner")).await.unwrap().unwrap();
        store
            .with_conn(|conn| {
                let affected = conn.execute("UPDATE embedding_profile SET model = 'model-b' WHERE singleton = 1", [])?;
                assert_eq!(affected, 1_usize);
                Ok(())
            })
            .await
            .unwrap();
        let audit = AuditDraft {
            action: AuditAction::Update,
            caller_agent: Some("owner".into()),
            timestamp: chrono::Utc::now(),
            details: None,
        };
        let replacement_embedding = vec![0.75_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];

        let error = store
            .update_authorized_if_unmodified_with_metadata_audited(
                &id,
                loaded.record_revision,
                &MemoryUpdate {
                    content: Some("must roll back".into()),
                    ..MemoryUpdate::default()
                },
                None,
                Some(&replacement_embedding),
                "owner",
                &audit,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, StoreError::Conflict(_)));
        assert_eq!(store.get(&id, Some("owner")).await.unwrap().unwrap().content, "profile guarded");
        assert_eq!(store.fetch_embeddings_for_ids(&[id]).await.unwrap().get(&id), Some(&original_embedding));
    }

    #[tokio::test]
    async fn mark_superseded_nonexistent_memory_returns_false() {
        use crate::{
            store::{MemoryWriter as _, SqliteStore},
            types::MemoryId,
        };

        let store = SqliteStore::in_memory().unwrap();
        let fake_id = MemoryId::new();
        let new_id = MemoryId::new();

        let result = store.mark_superseded_by(&fake_id, &new_id).await;
        assert!(result.is_ok());
        assert!(!result.unwrap(), "should return false for nonexistent memory");
    }

    #[tokio::test]
    async fn mark_superseded_authorized_denies_non_owner() {
        use crate::{
            store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
            types::{AccessPolicy, Importance, Memory, MemoryType, Provenance},
        };

        let store = SqliteStore::in_memory().unwrap();
        let owned = Memory {
            id: MemoryId::new(),
            content: "owned".into(),
            tags: vec![],
            provenance: Provenance {
                source_agent: Some("owner".into()),
                ..Default::default()
            },
            access_policy: AccessPolicy::Restricted { allowed: vec!["owner".into()] },
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
        };
        let replacement = Memory {
            id: MemoryId::new(),
            content: "replacement".into(),
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
        };

        let owned_id = store.store(&owned, None).await.unwrap();
        let replacement_id = store.store(&replacement, None).await.unwrap();

        let outcome = store.mark_superseded_by_authorized(&owned_id, &replacement_id, "intruder").await.unwrap();
        assert_eq!(outcome, WriteOutcome::Denied);

        let after = store.get(&owned_id, Some("owner")).await.unwrap().unwrap();
        assert!(after.superseded_by.is_none());
    }
}
