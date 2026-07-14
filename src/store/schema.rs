//! DDL definitions and schema migrations for the memories database.

use rusqlite::{Connection, OptionalExtension as _};

use crate::error::StoreError;

/// Current on-disk SQLite schema contract.
///
/// This project reset its pre-1.0 schema lineage. Databases carrying a newer
/// value are never opened or restored by an older binary.
pub(crate) const SQLITE_SCHEMA_VERSION: u32 = 2;

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

    conn.execute_batch(
        "
        BEGIN;
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
        COMMIT;
        ",
    )?;
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

/// DDL for the scope registry.
pub(crate) const SCOPE_REGISTRY_DDL: &str = "
    CREATE TABLE IF NOT EXISTS scope_registry (
        scope_key    TEXT PRIMARY KEY,
        display_name TEXT NOT NULL,
        description  TEXT,
        aliases      TEXT NOT NULL DEFAULT '[]',
        matchers     TEXT NOT NULL DEFAULT '[]',
        parent       TEXT,
        related      TEXT NOT NULL DEFAULT '[]',
        updated_at   TEXT NOT NULL
    );
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

/// Create the scope registry table for fresh and existing databases.
pub(crate) fn migrate_create_scope_registry(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(SCOPE_REGISTRY_DDL)?;
    Ok(())
}

/// Create the metadata table for fresh and existing databases.
pub(crate) fn migrate_create_metadata(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(METADATA_DDL)?;
    Ok(())
}

/// Create the deleted-memory tombstone table for fresh and existing databases.
pub(crate) fn migrate_create_memory_tombstones(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(TOMBSTONE_DDL)?;
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
