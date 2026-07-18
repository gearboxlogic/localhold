-- fixture-include: v0.1.0-beta.2-beta.3.postgres.sql
ALTER TABLE memories ADD COLUMN record_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE memory_v2_metadata RENAME TO memory_metadata;
ALTER INDEX idx_memory_v2_metadata_scope_key RENAME TO idx_memory_metadata_scope_key;
UPDATE memory_metadata SET schema_version = 1;
ALTER TABLE memory_metadata ALTER COLUMN schema_version SET DEFAULT 1;
INSERT INTO localhold_migrations(version, name) VALUES (3, 'record_revision');
