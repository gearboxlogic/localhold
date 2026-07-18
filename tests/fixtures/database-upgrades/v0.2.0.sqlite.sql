PRAGMA foreign_keys = ON;
PRAGMA user_version = 2;

-- The v0.2.0 public schema is represented by the same minimal data surfaces as
-- the beta builder, with the durable contract changes applied below.
-- fixture-include: v0.1.0-beta.2-beta.3.sqlite.sql
ALTER TABLE memories ADD COLUMN record_revision INTEGER NOT NULL DEFAULT 0;
DROP TRIGGER trg_memory_clear_superseded_by;
CREATE TRIGGER trg_memory_clear_superseded_by AFTER DELETE ON memories BEGIN
    UPDATE memories
    SET superseded_by = NULL, record_revision = record_revision + 1
    WHERE superseded_by = OLD.id;
END;
ALTER TABLE memory_v2_metadata RENAME TO memory_metadata;
DROP INDEX idx_memory_v2_metadata_scope_key;
CREATE TABLE memory_metadata_v0_2 (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    scope_key TEXT,
    summary TEXT,
    agent_label TEXT,
    created_by_principal TEXT,
    quality_flags TEXT NOT NULL DEFAULT '[]',
    schema_version INTEGER NOT NULL DEFAULT 1,
    migrated_at TEXT,
    updated_at TEXT NOT NULL
);
INSERT INTO memory_metadata_v0_2
SELECT memory_id, scope_key, summary, agent_label, created_by_principal, quality_flags, 1, migrated_at, updated_at
FROM memory_metadata;
DROP TABLE memory_metadata;
ALTER TABLE memory_metadata_v0_2 RENAME TO memory_metadata;
CREATE INDEX idx_memory_metadata_scope_key ON memory_metadata(scope_key);
PRAGMA user_version = 2;
