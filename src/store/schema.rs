//! DDL definitions and schema migrations for the memories database.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::{
    context::{ContextId, ContextKind, LEGACY_ALL_PRINCIPALS_GRANT, LEGACY_SYSTEM_PRINCIPAL, OPERATOR_PRINCIPAL, UNRESOLVED_CONTEXT_KEY, effective_legacy_scope_key},
    error::StoreError,
    types::{ANONYMOUS_PRINCIPAL, AccessPolicy, Provenance, normalize_context_key},
};

/// Current on-disk SQLite schema contract.
///
/// This project reset its pre-1.0 schema lineage. Databases carrying a newer
/// value are never opened or restored by an older binary.
pub(crate) const SQLITE_SCHEMA_VERSION: u32 = 3;

/// Core DDL for the memories table and its indexes.
pub(crate) const MAIN_DDL: &str = "
    CREATE TABLE IF NOT EXISTS memories (
        id            TEXT PRIMARY KEY,
        content       TEXT NOT NULL,
        tags          TEXT NOT NULL,
        provenance    TEXT NOT NULL,
        access_policy TEXT NOT NULL,
        created_at    TEXT NOT NULL,
        expires_at    TEXT,
        has_embedding INTEGER NOT NULL DEFAULT 0,
        embedding_revision INTEGER NOT NULL DEFAULT 0,
        record_revision INTEGER NOT NULL DEFAULT 0,
        memory_type   TEXT NOT NULL DEFAULT 'semantic',
        importance    REAL NOT NULL DEFAULT 0.5,
        impression_count INTEGER NOT NULL DEFAULT 0,
        last_impressed_at TEXT,
        superseded_by TEXT,
        activity_mass REAL NOT NULL DEFAULT 0.0,
        last_used_at  TEXT,
        updated_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        confidence    REAL NOT NULL DEFAULT 0.8,
        embedding_claimed_at TEXT,
        embedding_claim_token TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_memories_created_at ON memories(created_at DESC);

    CREATE TABLE IF NOT EXISTS memory_embedding_map (
        memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
        vec_rowid INTEGER NOT NULL UNIQUE
    );

    CREATE TABLE IF NOT EXISTS embedding_profile (
        singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
        provider   TEXT NOT NULL,
        endpoint   TEXT NOT NULL,
        model      TEXT NOT NULL,
        dimensions INTEGER NOT NULL CHECK (dimensions > 0)
    );

    CREATE INDEX IF NOT EXISTS idx_memories_source_agent
        ON memories(json_extract(provenance, '$.source_agent'));

    CREATE INDEX IF NOT EXISTS idx_memories_source_conversation
        ON memories(json_extract(provenance, '$.source_conversation'));

    CREATE INDEX IF NOT EXISTS idx_memories_origin_conversation
        ON memories(json_extract(provenance, '$.origin_conversation'));

    CREATE INDEX IF NOT EXISTS idx_memories_effective_origin_conversation
        ON memories(COALESCE(json_extract(provenance, '$.origin_conversation'), json_extract(provenance, '$.source_conversation')));

    CREATE INDEX IF NOT EXISTS idx_memories_access_type
        ON memories(json_extract(access_policy, '$.type'));

    CREATE INDEX IF NOT EXISTS idx_memories_expires_at
        ON memories(expires_at) WHERE expires_at IS NOT NULL;

    CREATE INDEX IF NOT EXISTS idx_memories_has_embedding
        ON memories(has_embedding);

    CREATE INDEX IF NOT EXISTS idx_memories_embedding_claim
        ON memories(has_embedding, embedding_claimed_at, created_at, id)
        WHERE has_embedding = 0;

    CREATE INDEX IF NOT EXISTS idx_memories_memory_type
        ON memories(memory_type);

    CREATE INDEX IF NOT EXISTS idx_memories_superseded_by
        ON memories(superseded_by) WHERE superseded_by IS NOT NULL;

    CREATE TABLE IF NOT EXISTS memory_entities (
        memory_id   TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        entity      TEXT NOT NULL,
        entity_type TEXT NOT NULL,
        PRIMARY KEY (memory_id, entity, entity_type)
    );

    CREATE INDEX IF NOT EXISTS idx_memory_entities_entity
        ON memory_entities(entity);

    CREATE INDEX IF NOT EXISTS idx_memory_entities_entity_type
        ON memory_entities(entity_type);
";

/// Trigger to cascade embedding deletes when the mapping row is removed,
/// and to clear dangling `superseded_by` references when a superseding memory is deleted.
pub(crate) const TRIGGER_DDL: &str = "
    CREATE TRIGGER IF NOT EXISTS trg_memory_embedding_map_delete
    AFTER DELETE ON memory_embedding_map
    BEGIN
        DELETE FROM memory_embeddings WHERE rowid = OLD.vec_rowid;
    END;

    CREATE TRIGGER IF NOT EXISTS trg_memory_clear_superseded_by
    AFTER DELETE ON memories
    BEGIN
        UPDATE memories
        SET superseded_by = NULL, record_revision = record_revision + 1
        WHERE superseded_by = OLD.id;
    END;
";

/// DDL for the FTS5 external-content table and sync triggers.
///
/// Uses `content=memories` so FTS5 stores only the inverted index, not a copy of the text.
/// The `unicode61` tokenizer handles multilingual text with diacritics removal.
///
/// Three triggers keep the FTS5 index in sync with the `memories` table:
/// - `AFTER INSERT` — index new content
/// - `AFTER UPDATE OF content` — re-index changed content
/// - `BEFORE DELETE` — remove from index (must fire before `ON DELETE CASCADE` removes the row)
pub(crate) const FTS5_DDL: &str = "
    CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
        content,
        content=memories,
        content_rowid=rowid,
        tokenize='unicode61 remove_diacritics 2'
    );

    CREATE TRIGGER IF NOT EXISTS trg_memory_fts_insert
    AFTER INSERT ON memories
    BEGIN
        INSERT INTO memory_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
    END;

    CREATE TRIGGER IF NOT EXISTS trg_memory_fts_update
    AFTER UPDATE OF content ON memories
    BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, content) VALUES('delete', OLD.rowid, OLD.content);
        INSERT INTO memory_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
    END;

    CREATE TRIGGER IF NOT EXISTS trg_memory_fts_delete
    BEFORE DELETE ON memories
    BEGIN
        INSERT INTO memory_fts(memory_fts, rowid, content) VALUES('delete', OLD.rowid, OLD.content);
    END;
";

/// Warn if an existing vec0 table has a different dimension than configured.
pub(crate) fn check_dimension_mismatch(conn: &Connection, embedding_dimensions: usize) -> Result<(), StoreError> {
    let existing_dim = existing_embedding_dimensions(conn)?;

    if let Some(dim) = existing_dim
        && dim != embedding_dimensions
    {
        return Err(StoreError::Conflict(format!(
            "existing memory_embeddings table has {dim} dimensions but config specifies {embedding_dimensions}; \
             drop and recreate the database to change dimensions"
        )));
    }
    Ok(())
}

/// Read the dimensions declared by an existing sqlite-vec table.
pub(crate) fn existing_embedding_dimensions(conn: &Connection) -> Result<Option<usize>, StoreError> {
    let dimensions = conn
        .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='memory_embeddings'", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
        .and_then(|sql| parse_vec_dimensions(&sql));
    Ok(dimensions)
}

fn parse_vec_dimensions(sql: &str) -> Option<usize> {
    let start = sql.find("float[")?.checked_add(6)?;
    let end = start.checked_add(sql.get(start..)?.find(']')?)?;
    sql.get(start..end)?.parse().ok()
}

/// Add `embedding_revision` column to legacy databases that lack it.
pub(crate) fn migrate_memories_add_embedding_revision(conn: &Connection) -> Result<(), StoreError> {
    if has_column(conn, "embedding_revision")? {
        return Ok(());
    }
    #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
    conn.execute("ALTER TABLE memories ADD COLUMN embedding_revision INTEGER NOT NULL DEFAULT 0", [])?;
    Ok(())
}

/// Add the user-visible record revision used for optimistic concurrency.
pub(crate) fn migrate_memories_add_record_revision(conn: &Connection) -> Result<(), StoreError> {
    if has_column(conn, "record_revision")? {
        return Ok(());
    }
    #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
    conn.execute("ALTER TABLE memories ADD COLUMN record_revision INTEGER NOT NULL DEFAULT 0", [])?;
    Ok(())
}

/// Add re-embed claim lease columns and index to existing databases.
pub(crate) fn migrate_memories_add_embedding_claims(conn: &Connection) -> Result<(), StoreError> {
    if !has_column(conn, "embedding_claimed_at")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN embedding_claimed_at TEXT", [])?;
    }
    if !has_column(conn, "embedding_claim_token")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN embedding_claim_token TEXT", [])?;
    }
    #[expect(unused_results, reason = "CREATE INDEX DDL — row count is meaningless")]
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memories_embedding_claim
         ON memories(has_embedding, embedding_claimed_at, created_at, id)
         WHERE has_embedding = 0",
        [],
    )?;
    Ok(())
}

/// Backfill `origin_conversation` from `source_conversation` for legacy rows.
pub(crate) fn migrate_memories_backfill_origin_conversation(conn: &Connection) -> Result<(), StoreError> {
    let needs_backfill: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memories WHERE json_extract(provenance, '$.origin_conversation') IS NULL AND json_extract(provenance, '$.source_conversation') IS NOT NULL)",
        [],
        |row| row.get(0),
    )?;
    if !needs_backfill {
        return Ok(());
    }
    #[expect(unused_results, reason = "UPDATE migration — affected row count is not actionable")]
    conn.execute(
        "UPDATE memories
         SET provenance = json_set(
             provenance,
             '$.origin_conversation',
             json_extract(provenance, '$.source_conversation')
         )
         WHERE json_extract(provenance, '$.origin_conversation') IS NULL
           AND json_extract(provenance, '$.source_conversation') IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// Recreate `memory_embedding_map` with a proper foreign key, dropping orphaned rows.
pub(crate) fn migrate_memory_embedding_map_fk(conn: &Connection) -> Result<(), StoreError> {
    const MIGRATION: &str = "
        ALTER TABLE memory_embedding_map RENAME TO memory_embedding_map_old;
        CREATE TABLE memory_embedding_map (
            memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
            vec_rowid INTEGER NOT NULL UNIQUE
        );
        INSERT INTO memory_embedding_map(memory_id, vec_rowid)
        SELECT old.memory_id, old.vec_rowid
        FROM memory_embedding_map_old AS old
        JOIN memories ON memories.id = old.memory_id;
        DROP TABLE memory_embedding_map_old;
    ";
    let mut stmt = conn.prepare("PRAGMA foreign_key_list(memory_embedding_map)")?;
    let mut rows = stmt.query([])?;
    let mut has_fk = false;
    while let Some(row) = rows.next()? {
        let table: String = row.get(2)?;
        let from_col: String = row.get(3)?;
        if table == "memories" && from_col == "memory_id" {
            has_fk = true;
            break;
        }
    }
    if has_fk {
        return Ok(());
    }

    if conn.is_autocommit() {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        if let Err(error) = conn.execute_batch(MIGRATION) {
            let _rollback = conn.execute_batch("ROLLBACK");
            return Err(error.into());
        }
        conn.execute_batch("COMMIT")?;
    } else {
        conn.execute_batch(MIGRATION)?;
    }
    Ok(())
}

/// Create the FTS5 external-content table and sync triggers, backfilling
/// existing content if this is the first run on a pre-existing database.
///
/// Returns `true` if FTS5 is available, `false` if the extension is missing.
pub(crate) fn migrate_create_fts_index(conn: &Connection) -> Result<bool, StoreError> {
    // Check if memory_fts already exists. If it does, still re-run the IF NOT
    // EXISTS DDL so normal startup can repair missing FTS sync triggers.
    let fts_exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_fts')", [], |row| row.get(0))?;

    if fts_exists {
        match conn.execute_batch(FTS5_DDL) {
            Ok(()) => return Ok(true),
            Err(e) => {
                tracing::warn!("FTS5 extension unavailable, hybrid search disabled: {e}");
                return Ok(false);
            }
        }
    }

    // Attempt to create the FTS5 table + triggers. If FTS5 is not compiled in,
    // this will fail gracefully and we disable FTS features at runtime.
    match conn.execute_batch(FTS5_DDL) {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!("FTS5 extension unavailable, hybrid search disabled: {e}");
            return Ok(false);
        }
    }

    // Backfill existing memories into the FTS index.
    let backfill_count: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    if backfill_count > 0 {
        tracing::info!(count = backfill_count, "backfilling FTS5 index for existing memories");
        #[expect(unused_results, reason = "INSERT INTO ... SELECT backfill — row count is logged above")]
        conn.execute("INSERT INTO memory_fts(rowid, content) SELECT rowid, content FROM memories", [])?;
    }

    Ok(true)
}

// ---------------------------------------------------------------------------
// Wave 1 migrations — memory_type, importance, access tracking
// ---------------------------------------------------------------------------

/// Helper: check if a column exists in the memories table.
fn has_column(conn: &Connection, col_name: &str) -> Result<bool, StoreError> {
    let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == col_name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Add `memory_type TEXT NOT NULL DEFAULT 'semantic'` to existing databases.
pub(crate) fn migrate_memories_add_memory_type(conn: &Connection) -> Result<(), StoreError> {
    if has_column(conn, "memory_type")? {
        return Ok(());
    }
    #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
    conn.execute("ALTER TABLE memories ADD COLUMN memory_type TEXT NOT NULL DEFAULT 'semantic'", [])?;
    // Index is created by MAIN_DDL's CREATE INDEX IF NOT EXISTS, but ensure it exists for
    // databases that were created before this migration.
    #[expect(unused_results, reason = "CREATE INDEX DDL — row count is meaningless")]
    conn.execute("CREATE INDEX IF NOT EXISTS idx_memories_memory_type ON memories(memory_type)", [])?;
    Ok(())
}

/// Add `importance REAL NOT NULL DEFAULT 0.5` to existing databases.
pub(crate) fn migrate_memories_add_importance(conn: &Connection) -> Result<(), StoreError> {
    if has_column(conn, "importance")? {
        return Ok(());
    }
    #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
    conn.execute("ALTER TABLE memories ADD COLUMN importance REAL NOT NULL DEFAULT 0.5", [])?;
    Ok(())
}

/// Add `superseded_by TEXT` column and ensure its index exists.
pub(crate) fn migrate_memories_add_superseded_by(conn: &Connection) -> Result<(), StoreError> {
    if !has_column(conn, "superseded_by")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN superseded_by TEXT", [])?;
    }
    #[expect(unused_results, reason = "CREATE INDEX DDL — row count is meaningless")]
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memories_superseded_by ON memories(superseded_by) WHERE superseded_by IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// Helper: check if a table exists in the database.
fn has_table(conn: &Connection, table_name: &str) -> Result<bool, StoreError> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [table_name], |row| row.get(0))?;
    Ok(exists)
}

/// Create the `memory_entities` junction table for entity tagging on existing databases.
pub(crate) fn migrate_create_memory_entities(conn: &Connection) -> Result<(), StoreError> {
    if has_table(conn, "memory_entities")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_entities (
            memory_id   TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
            entity      TEXT NOT NULL,
            entity_type TEXT NOT NULL,
            PRIMARY KEY (memory_id, entity, entity_type)
        );
        CREATE INDEX IF NOT EXISTS idx_memory_entities_entity
            ON memory_entities(entity);
        CREATE INDEX IF NOT EXISTS idx_memory_entities_entity_type
            ON memory_entities(entity_type);",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Wave 4 migrations — memory audit log
// ---------------------------------------------------------------------------

/// DDL for the append-only audit log table.
pub(crate) const AUDIT_LOG_DDL: &str = "
    CREATE TABLE IF NOT EXISTS memory_audit_log (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        memory_id   TEXT NOT NULL,
        action      TEXT NOT NULL,
        caller_agent TEXT,
        timestamp   TEXT NOT NULL,
        details     TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_audit_log_memory_id
        ON memory_audit_log(memory_id);
    CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp
        ON memory_audit_log(timestamp DESC);
";

/// DDL for governed contexts and direct memory membership.
///
/// Identity rows contain only fingerprints and redacted labels. Raw identity
/// values never reach these tables.
pub(crate) const CONTEXT_DDL: &str = "
    CREATE TABLE IF NOT EXISTS context_kinds (
        kind         TEXT PRIMARY KEY,
        display_name TEXT NOT NULL,
        builtin      INTEGER NOT NULL DEFAULT 0 CHECK (builtin IN (0, 1)),
        enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS contexts (
        id              TEXT PRIMARY KEY,
        kind            TEXT NOT NULL REFERENCES context_kinds(kind) ON DELETE RESTRICT,
        context_key     TEXT NOT NULL,
        normalized_key  TEXT NOT NULL,
        display_name    TEXT NOT NULL,
        description     TEXT,
        owner_principal TEXT NOT NULL,
        guidance        TEXT,
        parent_id       TEXT REFERENCES contexts(id) ON DELETE RESTRICT,
        lifecycle       TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle IN ('active', 'archived')),
        frozen          INTEGER NOT NULL DEFAULT 0 CHECK (frozen IN (0, 1)),
        created_at      TEXT NOT NULL,
        updated_at      TEXT NOT NULL,
        UNIQUE (owner_principal, kind, normalized_key),
        CHECK (parent_id IS NULL OR parent_id <> id)
    );

    CREATE INDEX IF NOT EXISTS idx_contexts_owner_kind_key
        ON contexts(owner_principal, kind, normalized_key);
    CREATE INDEX IF NOT EXISTS idx_contexts_kind_key
        ON contexts(kind, normalized_key, id);
    CREATE INDEX IF NOT EXISTS idx_contexts_key
        ON contexts(normalized_key, id);
    CREATE INDEX IF NOT EXISTS idx_contexts_parent
        ON contexts(parent_id) WHERE parent_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_contexts_lifecycle
        ON contexts(lifecycle, kind);

    CREATE TABLE IF NOT EXISTS context_aliases (
        context_id      TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        alias           TEXT NOT NULL,
        normalized_alias TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        PRIMARY KEY (context_id, normalized_alias)
    );

    CREATE INDEX IF NOT EXISTS idx_context_aliases_lookup
        ON context_aliases(normalized_alias, context_id);

    CREATE TABLE IF NOT EXISTS context_identities (
        context_id      TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        owner_principal TEXT NOT NULL,
        kind            TEXT NOT NULL,
        scheme          TEXT NOT NULL,
        namespace       TEXT NOT NULL DEFAULT '',
        fingerprint     TEXT NOT NULL,
        redacted_label  TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        PRIMARY KEY (context_id, scheme, namespace, fingerprint),
        UNIQUE (owner_principal, kind, scheme, namespace, fingerprint)
    );

    CREATE INDEX IF NOT EXISTS idx_context_identities_lookup
        ON context_identities(owner_principal, kind, scheme, namespace, fingerprint);
    CREATE INDEX IF NOT EXISTS idx_context_identities_exact
        ON context_identities(kind, scheme, namespace, fingerprint, context_id);

    CREATE TABLE IF NOT EXISTS context_resolver_hints (
        context_id      TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        hint            TEXT NOT NULL,
        normalized_hint TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        PRIMARY KEY (context_id, normalized_hint)
    );

    CREATE INDEX IF NOT EXISTS idx_context_resolver_hints_lookup
        ON context_resolver_hints(normalized_hint, context_id);

    CREATE TABLE IF NOT EXISTS context_grants (
        context_id        TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        grantee_principal TEXT NOT NULL,
        granted_by        TEXT NOT NULL,
        created_at        TEXT NOT NULL,
        PRIMARY KEY (context_id, grantee_principal)
    );

    CREATE INDEX IF NOT EXISTS idx_context_grants_principal
        ON context_grants(grantee_principal, context_id);

    CREATE TABLE IF NOT EXISTS context_relations (
        from_context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        to_context_id   TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        relation        TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        PRIMARY KEY (from_context_id, to_context_id, relation),
        CHECK (from_context_id <> to_context_id)
    );

    CREATE INDEX IF NOT EXISTS idx_context_relations_reverse
        ON context_relations(to_context_id, relation, from_context_id);

    CREATE TABLE IF NOT EXISTS memory_contexts (
        memory_id  TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE RESTRICT,
        ordinal    INTEGER NOT NULL CHECK (ordinal >= 0),
        created_at TEXT NOT NULL,
        PRIMARY KEY (memory_id, context_id),
        UNIQUE (memory_id, ordinal)
    );

    CREATE INDEX IF NOT EXISTS idx_memory_contexts_memory
        ON memory_contexts(memory_id, ordinal, context_id);
    CREATE INDEX IF NOT EXISTS idx_memory_contexts_context
        ON memory_contexts(context_id, memory_id);

    CREATE TABLE IF NOT EXISTS context_kind_policies (
        layer       TEXT NOT NULL CHECK (layer IN ('operator', 'principal')),
        principal   TEXT NOT NULL DEFAULT '',
        kind        TEXT NOT NULL REFERENCES context_kinds(kind) ON DELETE RESTRICT,
        policy_json TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        PRIMARY KEY (layer, principal, kind),
        CHECK ((layer = 'operator' AND principal = '') OR (layer = 'principal' AND principal <> ''))
    );

    CREATE TABLE IF NOT EXISTS context_anchor_overrides (
        anchor_context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
        principal         TEXT NOT NULL,
        policy_json       TEXT NOT NULL,
        updated_at        TEXT NOT NULL,
        PRIMARY KEY (anchor_context_id, principal)
    );

    CREATE TABLE IF NOT EXISTS context_audit_events (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_principal TEXT NOT NULL,
        action          TEXT NOT NULL,
        context_id      TEXT,
        memory_id       TEXT,
        timestamp       TEXT NOT NULL,
        details         TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_context_audit_context
        ON context_audit_events(context_id, timestamp DESC)
        WHERE context_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_context_audit_memory
        ON context_audit_events(memory_id, timestamp DESC)
        WHERE memory_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_context_audit_timestamp
        ON context_audit_events(timestamp DESC);
";

/// DDL for non-destructive metadata attached to existing memories.
pub(crate) const METADATA_DDL: &str = "
    CREATE TABLE IF NOT EXISTS memory_metadata (
        memory_id            TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
        scope_key            TEXT,
        summary              TEXT,
        agent_label          TEXT,
        created_by_principal TEXT,
        quality_flags        TEXT NOT NULL DEFAULT '[]',
        schema_version       INTEGER NOT NULL DEFAULT 1,
        migrated_at          TEXT,
        updated_at           TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_memory_metadata_scope_key
        ON memory_metadata(scope_key);
";

/// DDL for deleted-memory authorization tombstones.
pub(crate) const TOMBSTONE_DDL: &str = "
    CREATE TABLE IF NOT EXISTS memory_tombstones (
        memory_id            TEXT PRIMARY KEY,
        provenance           TEXT NOT NULL,
        access_policy        TEXT NOT NULL,
        deleted_at           TEXT NOT NULL,
        deleted_by_principal TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_memory_tombstones_deleted_at
        ON memory_tombstones(deleted_at DESC);
";

#[derive(Debug)]
struct LegacyScope {
    key: String,
    kind: ContextKind,
    display_name: String,
    description: Option<String>,
    aliases: Vec<String>,
    hints: Vec<String>,
    parent: Option<String>,
    related: Vec<String>,
    updated_at: Option<String>,
    registered: bool,
    globally_visible: bool,
}

impl LegacyScope {
    fn raw(key: String) -> Self {
        let display_name = crate::context::legacy_scope_display_name(&key);
        Self {
            kind: ContextKind::custom(),
            key,
            display_name,
            description: Some("Migrated legacy compatibility scope".into()),
            aliases: Vec::new(),
            hints: Vec::new(),
            parent: None,
            related: Vec::new(),
            updated_at: None,
            registered: false,
            globally_visible: false,
        }
    }
}

/// Create the governed-context schema and migrate the SQLite v2 scope model.
///
/// This focused entry point is retained for transaction-level migration tests.
/// Production startup uses [`migrate_contexts_v3_validated`] so the complete
/// post-migration schema and data contract is checked before commit.
#[cfg(test)]
pub(crate) fn migrate_contexts_v3(conn: &mut Connection, source_version: u32, now: DateTime<Utc>) -> Result<(), StoreError> {
    migrate_contexts_v3_inner(conn, source_version, now, None)
}

/// Migrate and validate governed contexts inside a caller-owned SQLite upgrade
/// transaction.
///
/// This keeps published metadata rewrites and the context migration in one
/// atomic commit.
pub(crate) fn migrate_contexts_v3_validated_in_transaction(tx: &Transaction<'_>, source_version: u32, now: DateTime<Utc>, embedding_dimensions: usize) -> Result<(), StoreError> {
    migrate_contexts_v3_transaction(tx, source_version, now, Some(embedding_dimensions))
}

#[cfg(test)]
fn migrate_contexts_v3_inner(conn: &mut Connection, source_version: u32, now: DateTime<Utc>, embedding_dimensions: Option<usize>) -> Result<(), StoreError> {
    let current_version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if current_version > SQLITE_SCHEMA_VERSION {
        return Err(StoreError::Conflict(format!(
            "SQLite schema version {current_version} is newer than this binary supports ({SQLITE_SCHEMA_VERSION})"
        )));
    }
    let tx = if current_version == SQLITE_SCHEMA_VERSION {
        conn.transaction()?
    } else {
        super::sqlite::sqlite_write_tx(conn)?
    };
    migrate_contexts_v3_transaction(&tx, source_version, now, embedding_dimensions)?;
    tx.commit()?;
    Ok(())
}

fn migrate_contexts_v3_transaction(tx: &Transaction<'_>, _source_version: u32, now: DateTime<Utc>, embedding_dimensions: Option<usize>) -> Result<(), StoreError> {
    let locked_version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if locked_version > SQLITE_SCHEMA_VERSION {
        return Err(StoreError::Conflict(format!(
            "SQLite schema version {locked_version} is newer than this binary supports ({SQLITE_SCHEMA_VERSION})"
        )));
    }
    if locked_version == SQLITE_SCHEMA_VERSION {
        let registry_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'scope_registry')", [], |row| {
            row.get(0)
        })?;
        if registry_exists {
            return Err(StoreError::Conflict(
                "SQLite v3 contains retired scope_registry; remove the stray table or restore a valid current backup".into(),
            ));
        }
        if let Some(embedding_dimensions) = embedding_dimensions {
            crate::store::migration::validate_sqlite_source_schema(tx, embedding_dimensions)?;
        }
        return Ok(());
    }
    let registry_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'scope_registry')", [], |row| {
        row.get(0)
    })?;
    if locked_version == 2 && !registry_exists {
        return Err(StoreError::Conflict(
            "SQLite v2 database is missing scope_registry; restore from backup or repair the schema before retrying".into(),
        ));
    }
    if registry_exists {
        validate_legacy_scope_registry(tx)?;
    }

    tx.execute_batch(CONTEXT_DDL)?;
    insert_builtin_context_kinds(tx, &now.to_rfc3339())?;

    migrate_legacy_scopes(tx, &now.to_rfc3339(), registry_exists)?;
    if registry_exists {
        let _dropped = tx.execute("DROP TABLE scope_registry", [])?;
    }

    tx.execute_batch(TOMBSTONE_DDL)?;
    tx.pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION)?;
    if let Some(embedding_dimensions) = embedding_dimensions {
        crate::store::migration::validate_sqlite_source_schema(tx, embedding_dimensions)?;
    }
    Ok(())
}

#[expect(clippy::type_complexity, reason = "the compact tuple mirrors SQLite PRAGMA table_info columns")]
fn validate_legacy_scope_registry(tx: &Transaction<'_>) -> Result<(), StoreError> {
    const EXPECTED: &[(&str, &str, bool, Option<&str>, i64)] = &[
        ("scope_key", "TEXT", false, None, 1),
        ("display_name", "TEXT", true, None, 0),
        ("description", "TEXT", false, None, 0),
        ("aliases", "TEXT", true, Some("'[]'"), 0),
        ("matchers", "TEXT", true, Some("'[]'"), 0),
        ("parent", "TEXT", false, None, 0),
        ("related", "TEXT", true, Some("'[]'"), 0),
        ("updated_at", "TEXT", true, None, 0),
    ];
    let mut statement = tx.prepare("PRAGMA table_info(scope_registry)")?;
    let actual = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if actual.len() != EXPECTED.len()
        || actual.iter().zip(EXPECTED).any(|(actual, expected)| {
            actual.0 != expected.0 || !actual.1.eq_ignore_ascii_case(expected.1) || actual.2 != expected.2 || actual.3.as_deref() != expected.3 || actual.4 != expected.4
        })
    {
        return Err(StoreError::Conflict("legacy scope_registry does not match the supported SQLite v2 contract".into()));
    }
    let invalid_rows: i64 = tx.query_row(
        "SELECT COUNT(*)
         FROM scope_registry
         WHERE trim(scope_key) = ''
            OR trim(display_name) = ''
            OR NOT json_valid(aliases) OR json_type(aliases) != 'array'
            OR NOT json_valid(matchers) OR json_type(matchers) != 'array'
            OR NOT json_valid(related) OR json_type(related) != 'array'",
        [],
        |row| row.get(0),
    )?;
    if invalid_rows != 0 {
        return Err(StoreError::Conflict("legacy scope_registry contains blank keys/names or malformed JSON arrays".into()));
    }
    let mut timestamps = tx.prepare("SELECT updated_at FROM scope_registry")?;
    let invalid_timestamp = timestamps
        .query_map([], |row| row.get::<_, String>(0))?
        .any(|timestamp| timestamp.map_or(true, |value| DateTime::parse_from_rfc3339(&value).is_err()));
    if invalid_timestamp {
        return Err(StoreError::Conflict(
            "legacy scope_registry contains an updated_at value that is not an RFC3339 timestamp".into(),
        ));
    }
    Ok(())
}

fn insert_builtin_context_kinds(tx: &Transaction<'_>, now: &str) -> Result<(), StoreError> {
    for (kind, display_name) in [
        (ContextKind::PROJECT, "Project"),
        (ContextKind::DOMAIN, "Domain"),
        (ContextKind::ORGANIZATION, "Organization"),
        (ContextKind::CUSTOM, "Custom"),
    ] {
        let _inserted = tx.execute(
            "INSERT INTO context_kinds (kind, display_name, builtin, enabled, created_at, updated_at)
             VALUES (?1, ?2, 1, 1, ?3, ?3)
             ON CONFLICT(kind) DO NOTHING",
            params![kind, display_name, now],
        )?;
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "the legacy scope backfill is one auditable all-or-nothing migration")]
fn migrate_legacy_scopes(tx: &Transaction<'_>, now: &str, registry_exists: bool) -> Result<(), StoreError> {
    validate_legacy_memory_json_sqlite(tx)?;
    let mut scopes = BTreeMap::<String, LegacyScope>::new();
    if registry_exists {
        let mut statement = tx.prepare(
            "SELECT scope_key, display_name, description, aliases, matchers, parent, related, updated_at
             FROM scope_registry
             ORDER BY scope_key",
        )?;
        let rows = statement.query_map([], |row| {
            let aliases: String = row.get(3)?;
            let hints: String = row.get(4)?;
            let related: String = row.get(6)?;
            Ok(LegacyScope {
                key: row.get(0)?,
                kind: ContextKind::from_legacy_scope(&row.get::<_, String>(0)?),
                display_name: row.get(1)?,
                description: row.get(2)?,
                aliases: serde_json::from_str(&aliases).map_err(|error| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error)))?,
                hints: serde_json::from_str(&hints).map_err(|error| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error)))?,
                parent: row.get(5)?,
                related: serde_json::from_str(&related).map_err(|error| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error)))?,
                updated_at: row.get(7)?,
                registered: true,
                globally_visible: true,
            })
        })?;
        for result in rows {
            let scope = result?;
            let normalized = normalize_context_key(&scope.key);
            if normalized == UNRESOLVED_CONTEXT_KEY {
                continue;
            }
            validate_sqlite_legacy_migration_scope(&scope)?;
            if normalized.is_empty() {
                return Err(StoreError::Conflict("legacy scope registry contains a blank key".into()));
            }
            if let Some(existing) = scopes.insert(normalized.clone(), scope)
                && existing.key != scopes[&normalized].key
            {
                return Err(StoreError::Conflict(format!(
                    "legacy scope keys {:?} and {:?} normalize to the same governed context",
                    existing.key, scopes[&normalized].key
                )));
            }
        }
    }

    let mut referenced_keys = Vec::new();
    for scope in scopes.values() {
        referenced_keys.extend(scope.parent.iter().cloned());
        referenced_keys.extend(scope.related.iter().cloned());
    }
    for key in referenced_keys {
        insert_raw_legacy_scope(&mut scopes, key, true)?;
    }

    {
        let mut statement = tx.prepare(
            "SELECT DISTINCT meta.scope_key,
                    json_extract(memory.provenance, '$.source_conversation')
             FROM memories AS memory
             LEFT JOIN memory_metadata AS meta ON meta.memory_id = memory.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)))?;
        let mut raw_keys = rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|(metadata_scope, provenance_scope)| effective_legacy_scope_key(metadata_scope.as_deref(), provenance_scope.as_deref()))
            .collect::<Vec<_>>();
        raw_keys.sort();
        for key in raw_keys {
            if normalize_context_key(&key) != UNRESOLVED_CONTEXT_KEY {
                insert_raw_legacy_scope(&mut scopes, key, false)?;
            }
        }
    }

    let mut ids = HashMap::<String, ContextId>::new();
    for (normalized_key, scope) in &scopes {
        let id = ContextId::new();
        let context_timestamp = scope.updated_at.as_deref().unwrap_or(now);
        let _inserted = tx.execute(
            "INSERT INTO contexts (
                id, kind, context_key, normalized_key, display_name, description,
                owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 'active', 1, ?8, ?8)",
            params![
                id.to_string(),
                scope.kind.as_str(),
                scope.key,
                normalized_key,
                scope.display_name,
                scope.description,
                LEGACY_SYSTEM_PRINCIPAL,
                context_timestamp,
            ],
        )?;
        if scope.globally_visible {
            let _granted = tx.execute(
                "INSERT INTO context_grants (context_id, grantee_principal, granted_by, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), LEGACY_ALL_PRINCIPALS_GRANT, LEGACY_SYSTEM_PRINCIPAL, now],
            )?;
        }
        for alias in &scope.aliases {
            let normalized = normalize_context_key(alias);
            if !normalized.is_empty() {
                let _inserted = tx.execute(
                    "INSERT OR IGNORE INTO context_aliases (context_id, alias, normalized_alias, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id.to_string(), alias, normalized, now],
                )?;
            }
        }
        for hint in &scope.hints {
            let normalized = normalize_context_key(hint);
            if !normalized.is_empty() {
                let _inserted = tx.execute(
                    "INSERT OR IGNORE INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id.to_string(), hint, normalized, now],
                )?;
            }
        }
        let audit_action = if scope.registered { "migrate_registered_scope" } else { "migrate_raw_scope" };
        let _audited = tx.execute(
            "INSERT INTO context_audit_events (
                actor_principal, action, context_id, memory_id, timestamp, details
             ) VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
            params![
                LEGACY_SYSTEM_PRINCIPAL,
                audit_action,
                id.to_string(),
                now,
                serde_json::json!({"legacy_scope_key": scope.key}).to_string(),
            ],
        )?;
        let _previous = ids.insert(normalized_key.clone(), id);
    }

    validate_legacy_parent_graph(&scopes)?;
    for (normalized_key, scope) in &scopes {
        if let Some(parent_key) = &scope.parent {
            let parent_normalized = normalize_context_key(parent_key);
            if parent_normalized == UNRESOLVED_CONTEXT_KEY {
                continue;
            }
            let parent_id = ids
                .get(&parent_normalized)
                .ok_or_else(|| StoreError::Conflict(format!("legacy scope {:?} references missing parent {parent_key:?}", scope.key)))?;
            let _updated = tx.execute("UPDATE contexts SET parent_id = ?1 WHERE id = ?2", params![
                parent_id.to_string(),
                ids[normalized_key].to_string()
            ])?;
        }
        for related_key in &scope.related {
            let related_normalized = normalize_context_key(related_key);
            if related_normalized == UNRESOLVED_CONTEXT_KEY || related_normalized == *normalized_key {
                continue;
            }
            let related_id = ids
                .get(&related_normalized)
                .ok_or_else(|| StoreError::Conflict(format!("legacy scope {:?} references missing related scope {related_key:?}", scope.key)))?;
            let _inserted = tx.execute(
                "INSERT OR IGNORE INTO context_relations (
                    from_context_id, to_context_id, relation, created_at
                 ) VALUES (?1, ?2, 'legacy_related', ?3)",
                params![ids[normalized_key].to_string(), related_id.to_string(), now],
            )?;
        }
    }

    let mut memberships = Vec::<(String, String)>::new();
    {
        let mut statement = tx.prepare(
            "SELECT memory.id, meta.scope_key,
                    json_extract(memory.provenance, '$.source_conversation')
             FROM memories AS memory
             LEFT JOIN memory_metadata AS meta ON meta.memory_id = memory.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?)))?;
        for result in rows {
            let (memory_id, metadata_scope, provenance_scope) = result?;
            let Some(scope_key) = effective_legacy_scope_key(metadata_scope.as_deref(), provenance_scope.as_deref()) else {
                continue;
            };
            let normalized = normalize_context_key(&scope_key);
            if normalized.is_empty() || normalized == UNRESOLVED_CONTEXT_KEY {
                continue;
            }
            memberships.push((memory_id, normalized));
        }
    }
    for (memory_id, normalized_key) in memberships {
        let context_id = ids
            .get(&normalized_key)
            .ok_or_else(|| StoreError::Conflict(format!("legacy memory {memory_id} references scope missing from context backfill")))?;
        let canonical_key = &scopes
            .get(&normalized_key)
            .ok_or_else(|| StoreError::Conflict(format!("legacy memory {memory_id} references scope missing from canonical context backfill")))?
            .key;
        let _inserted = tx.execute(
            "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
             VALUES (?1, ?2, 0, ?3)",
            params![&memory_id, context_id.to_string(), now],
        )?;
        let _canonicalized = tx.execute("UPDATE memory_metadata SET scope_key = ?1 WHERE memory_id = ?2", params![canonical_key, memory_id])?;
        let _canonicalized_provenance = tx.execute(
            "UPDATE memories
             SET provenance = json_set(provenance, '$.source_conversation', ?1)
             WHERE id = ?2",
            params![canonical_key, memory_id],
        )?;
    }

    grant_migrated_raw_contexts_sqlite(tx, now)?;
    let _updated = tx.execute(
        "UPDATE memory_metadata
         SET scope_key = ?1, updated_at = ?2
         WHERE memory_id NOT IN (SELECT memory_id FROM memory_contexts)",
        params![UNRESOLVED_CONTEXT_KEY, now],
    )?;
    let _updated_provenance = tx.execute(
        "UPDATE memories
         SET provenance = json_set(provenance, '$.source_conversation', ?1)
         WHERE id NOT IN (SELECT memory_id FROM memory_contexts)",
        [UNRESOLVED_CONTEXT_KEY],
    )?;
    Ok(())
}

fn validate_legacy_memory_json_sqlite(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let mut statement = tx.prepare("SELECT id, provenance, access_policy FROM memories ORDER BY id")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
    for row in rows {
        let (memory_id, provenance, access_policy) = row?;
        let _provenance = serde_json::from_str::<serde_json::Value>(&provenance)
            .ok()
            .filter(serde_json::Value::is_object)
            .and_then(|value| serde_json::from_value::<Provenance>(value).ok())
            .ok_or_else(|| StoreError::Conflict(format!("legacy memory {memory_id} contains malformed provenance JSON")))?;
        let _access_policy = serde_json::from_str::<serde_json::Value>(&access_policy)
            .ok()
            .filter(serde_json::Value::is_object)
            .and_then(|value| serde_json::from_value::<AccessPolicy>(value).ok())
            .ok_or_else(|| StoreError::Conflict(format!("legacy memory {memory_id} contains malformed access_policy JSON")))?;
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "the three access-policy grant derivations remain adjacent for migration review")]
fn grant_migrated_raw_contexts_sqlite(tx: &Transaction<'_>, now: &str) -> Result<(), StoreError> {
    let _broad_grants = tx.execute(
        "INSERT OR IGNORE INTO context_grants (
             context_id, grantee_principal, granted_by, created_at
         )
         SELECT DISTINCT membership.context_id, ?1, ?2, ?3
         FROM memory_contexts AS membership
         JOIN memories AS memory ON memory.id = membership.memory_id
         JOIN context_audit_events AS audit
           ON audit.context_id = membership.context_id
          AND audit.action = 'migrate_raw_scope'
         WHERE json_extract(memory.access_policy, '$.type') = 'public'
            OR (
                json_extract(memory.access_policy, '$.type') = 'redacted'
                AND EXISTS (
                    SELECT 1
                    FROM json_each(memory.access_policy, '$.visible_fields') AS visible
                    WHERE visible.value = 'provenance'
                )
            )",
        params![LEGACY_ALL_PRINCIPALS_GRANT, LEGACY_SYSTEM_PRINCIPAL, now],
    )?;
    let _owner_grants = tx.execute(
        "INSERT OR IGNORE INTO context_grants (
             context_id, grantee_principal, granted_by, created_at
         )
         SELECT DISTINCT membership.context_id,
                json_extract(memory.provenance, '$.source_agent'),
                ?1,
                ?2
         FROM memory_contexts AS membership
         JOIN memories AS memory ON memory.id = membership.memory_id
         JOIN context_audit_events AS audit
           ON audit.context_id = membership.context_id
          AND audit.action = 'migrate_raw_scope'
         WHERE trim(COALESCE(json_extract(memory.provenance, '$.source_agent'), '')) <> ''
           AND json_extract(memory.provenance, '$.source_agent') NOT IN (?3, ?4, ?5, ?6)
           AND NOT EXISTS (
               SELECT 1 FROM context_grants AS grant_row
               WHERE grant_row.context_id = membership.context_id
                 AND grant_row.grantee_principal = ?3
           )",
        params![
            LEGACY_SYSTEM_PRINCIPAL,
            now,
            LEGACY_ALL_PRINCIPALS_GRANT,
            OPERATOR_PRINCIPAL,
            LEGACY_SYSTEM_PRINCIPAL,
            ANONYMOUS_PRINCIPAL,
        ],
    )?;
    let _restricted_grants = tx.execute(
        "INSERT OR IGNORE INTO context_grants (
             context_id, grantee_principal, granted_by, created_at
         )
         SELECT DISTINCT membership.context_id, allowed.value, ?1, ?2
         FROM memory_contexts AS membership
         JOIN memories AS memory ON memory.id = membership.memory_id
         JOIN context_audit_events AS audit
           ON audit.context_id = membership.context_id
          AND audit.action = 'migrate_raw_scope'
         JOIN json_each(memory.access_policy, '$.allowed') AS allowed
         WHERE json_extract(memory.access_policy, '$.type') = 'restricted'
           AND allowed.type = 'text'
           AND trim(allowed.value) <> ''
           AND allowed.value NOT IN (?3, ?4, ?5, ?6)
           AND NOT EXISTS (
               SELECT 1 FROM context_grants AS grant_row
               WHERE grant_row.context_id = membership.context_id
                 AND grant_row.grantee_principal = ?3
           )",
        params![
            LEGACY_SYSTEM_PRINCIPAL,
            now,
            LEGACY_ALL_PRINCIPALS_GRANT,
            OPERATOR_PRINCIPAL,
            LEGACY_SYSTEM_PRINCIPAL,
            ANONYMOUS_PRINCIPAL,
        ],
    )?;
    Ok(())
}

fn insert_raw_legacy_scope(scopes: &mut BTreeMap<String, LegacyScope>, key: String, globally_visible: bool) -> Result<(), StoreError> {
    if key.trim().is_empty() {
        return Err(StoreError::Conflict("legacy scope cannot be migrated because its key is blank".into()));
    }
    let normalized = normalize_context_key(&key);
    if !normalized.is_empty() && normalized != UNRESOLVED_CONTEXT_KEY {
        if !globally_visible
            && let Some(existing) = scopes.get(&normalized)
            && !existing.registered
            && existing.key != key
        {
            return Err(StoreError::Conflict(format!(
                "legacy raw scope keys {:?} and {:?} normalize to the same governed context",
                existing.key, key
            )));
        }
        let scope = scopes.entry(normalized).or_insert_with(|| LegacyScope::raw(key));
        scope.globally_visible |= globally_visible;
    }
    Ok(())
}

fn validate_sqlite_legacy_migration_scope(scope: &LegacyScope) -> Result<(), StoreError> {
    let invalid = scope.key.trim().is_empty()
        || scope.display_name.trim().is_empty()
        || scope.parent.as_deref().is_some_and(|value| value.trim().is_empty())
        || scope.aliases.iter().chain(&scope.hints).chain(&scope.related).any(|value| value.trim().is_empty());
    if invalid {
        return Err(StoreError::Conflict(
            "legacy scope registry contains a blank key, name, parent, alias, matcher, or relation".into(),
        ));
    }
    Ok(())
}

fn validate_legacy_parent_graph(scopes: &BTreeMap<String, LegacyScope>) -> Result<(), StoreError> {
    for start in scopes.keys() {
        let mut visited = HashSet::new();
        let mut cursor = Some(start.as_str());
        while let Some(key) = cursor {
            if !visited.insert(key.to_owned()) {
                return Err(StoreError::Conflict(format!(
                    "legacy scope hierarchy contains a cycle involving {:?}; no context migration was applied",
                    scopes.get(key).map_or(key, |scope| scope.key.as_str())
                )));
            }
            cursor = scopes
                .get(key)
                .and_then(|scope| scope.parent.as_deref())
                .map(normalize_context_key)
                .and_then(|parent| scopes.get_key_value(&parent).map(|(key, _scope)| key.as_str()));
        }
    }
    Ok(())
}

/// Create the metadata table for fresh and existing databases.
pub(crate) fn migrate_create_metadata(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(METADATA_DDL)?;
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "the fixed published schema contract is clearest as one auditable validation unit")]
pub(crate) fn validate_published_v2_metadata(conn: &Connection) -> Result<bool, StoreError> {
    if !has_table(conn, "memory_v2_metadata")? {
        return Ok(false);
    }
    if has_table(conn, "memory_metadata")? {
        return Err(StoreError::Conflict(
            "SQLite contains both memory_v2_metadata and memory_metadata; restore the pre-upgrade backup or repair the conflicting tables before retrying".into(),
        ));
    }
    let columns = {
        let mut statement = conn.prepare("PRAGMA table_info('memory_v2_metadata')")?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let expected_columns = vec![
        (0_i64, "memory_id".into(), "TEXT".into(), false, None, 1_i64),
        (1_i64, "scope_key".into(), "TEXT".into(), false, None, 0_i64),
        (2_i64, "summary".into(), "TEXT".into(), false, None, 0_i64),
        (3_i64, "agent_label".into(), "TEXT".into(), false, None, 0_i64),
        (4_i64, "created_by_principal".into(), "TEXT".into(), false, None, 0_i64),
        (5_i64, "quality_flags".into(), "TEXT".into(), true, Some("'[]'".into()), 0_i64),
        (6_i64, "schema_version".into(), "INTEGER".into(), true, Some("2".into()), 0_i64),
        (7_i64, "migrated_at".into(), "TEXT".into(), false, None, 0_i64),
        (8_i64, "updated_at".into(), "TEXT".into(), true, None, 0_i64),
    ];
    if columns != expected_columns {
        return Err(StoreError::Conflict(
            "SQLite published-release metadata table has an unexpected table contract; the pre-upgrade backup was retained".into(),
        ));
    }
    let foreign_keys = {
        let mut statement = conn.prepare("PRAGMA foreign_key_list('memory_v2_metadata')")?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let expected_foreign_keys = vec![(
        0_i64,
        0_i64,
        "memories".into(),
        "memory_id".into(),
        "id".into(),
        "NO ACTION".into(),
        "CASCADE".into(),
        "NONE".into(),
    )];
    if foreign_keys != expected_foreign_keys {
        return Err(StoreError::Conflict(
            "SQLite published-release metadata table has unexpected foreign keys; the pre-upgrade backup was retained".into(),
        ));
    }
    let indexes = {
        let mut statement = conn.prepare("SELECT name, \"unique\", origin, partial FROM pragma_index_list('memory_v2_metadata') ORDER BY name")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?, row.get::<_, String>(2)?, row.get::<_, bool>(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let expected_indexes = vec![
        ("idx_memory_v2_metadata_scope_key".into(), false, "c".into(), false),
        ("sqlite_autoindex_memory_v2_metadata_1".into(), true, "pk".into(), false),
    ];
    let scope_index = {
        let mut statement = conn.prepare(
            "SELECT seqno, cid, name, \"desc\", coll, key
             FROM pragma_index_xinfo('idx_memory_v2_metadata_scope_key')
             ORDER BY seqno",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let expected_scope_index = vec![
        (0_i64, 1_i64, Some("scope_key".into()), false, Some("BINARY".into()), true),
        (1_i64, -1_i64, None, false, Some("BINARY".into()), false),
    ];
    if indexes != expected_indexes || scope_index != expected_scope_index {
        return Err(StoreError::Conflict(
            "SQLite published-release metadata table has unexpected indexes; the pre-upgrade backup was retained".into(),
        ));
    }
    let invalid_versions: i64 = conn.query_row("SELECT COUNT(*) FROM memory_v2_metadata WHERE schema_version IS NULL OR schema_version <> 2", [], |row| {
        row.get(0)
    })?;
    if invalid_versions != 0 {
        return Err(StoreError::Conflict(
            "SQLite published-release metadata table contains an unexpected schema_version; the pre-upgrade backup was retained".into(),
        ));
    }
    let invalid_quality_flags: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_v2_metadata
         WHERE quality_flags IS NULL
            OR CASE WHEN json_valid(quality_flags) THEN json_type(quality_flags) <> 'array' ELSE 1 END
            OR EXISTS (
                SELECT 1
                FROM json_each(
                    CASE WHEN json_valid(quality_flags) AND json_type(quality_flags) = 'array' THEN quality_flags ELSE '[]' END
                )
                WHERE json_each.type <> 'text'
            )",
        [],
        |row| row.get(0),
    )?;
    if invalid_quality_flags != 0 {
        return Err(StoreError::Conflict(
            "SQLite published-release metadata contains malformed quality_flags; expected a JSON array of strings and retained the pre-upgrade backup".into(),
        ));
    }
    Ok(true)
}

/// Upgrade the metadata table written by the published beta releases.
///
/// The caller must hold an immediate transaction. Validation and the copy then
/// share one writer-locked snapshot, so malformed rows or a conflicting current
/// table leave the public-release schema untouched.
pub(crate) fn migrate_published_v2_metadata(conn: &Transaction<'_>) -> Result<(), StoreError> {
    if !validate_published_v2_metadata(conn)? {
        return Ok(());
    }

    conn.execute_batch(
        "CREATE TABLE memory_metadata (
             memory_id            TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
             scope_key            TEXT,
             summary              TEXT,
             agent_label          TEXT,
             created_by_principal TEXT,
             quality_flags        TEXT NOT NULL DEFAULT '[]',
             schema_version       INTEGER NOT NULL DEFAULT 1,
             migrated_at          TEXT,
             updated_at           TEXT NOT NULL
         );
         INSERT INTO memory_metadata (
             memory_id, scope_key, summary, agent_label, created_by_principal,
             quality_flags, schema_version, migrated_at, updated_at
         )
         SELECT memory_id, scope_key, summary, agent_label, created_by_principal,
                quality_flags, 1, migrated_at, updated_at
         FROM memory_v2_metadata;
         CREATE INDEX idx_memory_metadata_scope_key ON memory_metadata(scope_key);
         DROP TABLE memory_v2_metadata;",
    )?;
    Ok(())
}

/// Create the `memory_audit_log` table on existing databases.
pub(crate) fn migrate_create_audit_log(conn: &Connection) -> Result<(), StoreError> {
    if has_table(conn, "memory_audit_log")? {
        return Ok(());
    }
    conn.execute_batch(AUDIT_LOG_DDL)?;
    Ok(())
}

/// Align impression-tracking columns with the current ranking schema.
///
/// Supports legacy `access_*` columns, fresh `impression_*` columns, or
/// databases that have neither pair yet. Mixed states fail loudly.
pub(crate) fn migrate_memories_align_impression_tracking(conn: &Connection) -> Result<(), StoreError> {
    let has_old_count = has_column(conn, "access_count")?;
    let has_old_last = has_column(conn, "last_accessed_at")?;
    let has_new_count = has_column(conn, "impression_count")?;
    let has_new_last = has_column(conn, "last_impressed_at")?;

    match ((has_old_count, has_old_last), (has_new_count, has_new_last)) {
        ((false, false), (false, false)) => {
            #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
            conn.execute("ALTER TABLE memories ADD COLUMN impression_count INTEGER NOT NULL DEFAULT 0", [])?;
            #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
            conn.execute("ALTER TABLE memories ADD COLUMN last_impressed_at TEXT", [])?;
            Ok(())
        }
        ((true, true), (false, false)) => {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE memories RENAME COLUMN access_count TO impression_count;
                 ALTER TABLE memories RENAME COLUMN last_accessed_at TO last_impressed_at;
                 COMMIT;",
            )?;
            Ok(())
        }
        ((false, false), (true, true)) => Ok(()),
        _ => Err(StoreError::Conflict(
            "memories impression tracking columns are in a mixed state; expected either access_* or impression_* columns".into(),
        )),
    }
}

/// Add `activity_mass REAL NOT NULL DEFAULT 0.0` and `last_used_at TEXT` to existing databases.
pub(crate) fn migrate_memories_add_activity_tracking(conn: &Connection) -> Result<(), StoreError> {
    if !has_column(conn, "activity_mass")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN activity_mass REAL NOT NULL DEFAULT 0.0", [])?;
    }
    if !has_column(conn, "last_used_at")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN last_used_at TEXT", [])?;
    }
    Ok(())
}

/// Add `updated_at TEXT` column and backfill from `created_at`.
pub(crate) fn migrate_memories_add_updated_at(conn: &Connection) -> Result<(), StoreError> {
    if !has_column(conn, "updated_at")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN updated_at TEXT", [])?;
    }
    // Repair legacy rows and keep the runtime shape aligned with `Memory.updated_at`.
    #[expect(unused_results, reason = "UPDATE backfill — row count is not useful")]
    conn.execute("UPDATE memories SET updated_at = created_at WHERE updated_at IS NULL", [])?;
    Ok(())
}

/// Add `confidence REAL NOT NULL DEFAULT 0.8` to existing databases.
pub(crate) fn migrate_memories_add_confidence(conn: &Connection) -> Result<(), StoreError> {
    if !has_column(conn, "confidence")? {
        #[expect(unused_results, reason = "ALTER TABLE DDL — row count is meaningless")]
        conn.execute("ALTER TABLE memories ADD COLUMN confidence REAL NOT NULL DEFAULT 0.8", [])?;
    }
    Ok(())
}
