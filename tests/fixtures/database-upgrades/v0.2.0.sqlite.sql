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
CREATE INDEX idx_memory_metadata_scope_key ON memory_metadata(scope_key);
UPDATE memory_metadata SET schema_version = 1;
PRAGMA user_version = 2;
