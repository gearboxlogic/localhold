PRAGMA foreign_keys = ON;
PRAGMA user_version = 0;

CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    tags TEXT NOT NULL,
    provenance TEXT NOT NULL,
    access_policy TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    has_embedding INTEGER NOT NULL DEFAULT 0,
    embedding_revision INTEGER NOT NULL DEFAULT 0,
    memory_type TEXT NOT NULL DEFAULT 'semantic',
    importance REAL NOT NULL DEFAULT 0.5,
    impression_count INTEGER NOT NULL DEFAULT 0,
    last_impressed_at TEXT,
    superseded_by TEXT,
    activity_mass REAL NOT NULL DEFAULT 0.0,
    last_used_at TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    confidence REAL NOT NULL DEFAULT 0.8,
    embedding_claimed_at TEXT,
    embedding_claim_token TEXT
);
CREATE INDEX idx_memories_created_at ON memories(created_at DESC);
CREATE INDEX idx_memories_source_agent ON memories(json_extract(provenance, '$.source_agent'));
CREATE INDEX idx_memories_source_conversation ON memories(json_extract(provenance, '$.source_conversation'));
CREATE INDEX idx_memories_origin_conversation ON memories(json_extract(provenance, '$.origin_conversation'));
CREATE INDEX idx_memories_effective_origin_conversation ON memories(COALESCE(json_extract(provenance, '$.origin_conversation'), json_extract(provenance, '$.source_conversation')));
CREATE INDEX idx_memories_access_type ON memories(json_extract(access_policy, '$.type'));
CREATE INDEX idx_memories_expires_at ON memories(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX idx_memories_has_embedding ON memories(has_embedding);
CREATE INDEX idx_memories_embedding_claim ON memories(has_embedding, embedding_claimed_at, created_at, id) WHERE has_embedding = 0;
CREATE INDEX idx_memories_memory_type ON memories(memory_type);
CREATE INDEX idx_memories_superseded_by ON memories(superseded_by) WHERE superseded_by IS NOT NULL;

CREATE VIRTUAL TABLE memory_embeddings USING vec0(embedding float[3]);
CREATE TABLE memory_embedding_map (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    vec_rowid INTEGER NOT NULL UNIQUE
);
CREATE TRIGGER trg_memory_embedding_map_delete AFTER DELETE ON memory_embedding_map BEGIN
    DELETE FROM memory_embeddings WHERE rowid = OLD.vec_rowid;
END;
CREATE TRIGGER trg_memory_clear_superseded_by AFTER DELETE ON memories BEGIN
    UPDATE memories SET superseded_by = NULL WHERE superseded_by = OLD.id;
END;

CREATE VIRTUAL TABLE memory_fts USING fts5(content, content=memories, content_rowid=rowid, tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER trg_memory_fts_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memory_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
END;
CREATE TRIGGER trg_memory_fts_update AFTER UPDATE OF content ON memories BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content) VALUES('delete', OLD.rowid, OLD.content);
    INSERT INTO memory_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
END;
CREATE TRIGGER trg_memory_fts_delete BEFORE DELETE ON memories BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content) VALUES('delete', OLD.rowid, OLD.content);
END;

CREATE TABLE embedding_profile (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    provider TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL CHECK (dimensions > 0)
);
CREATE TABLE memory_entities (
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    entity TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    PRIMARY KEY (memory_id, entity, entity_type)
);
CREATE INDEX idx_memory_entities_entity ON memory_entities(entity);
CREATE INDEX idx_memory_entities_entity_type ON memory_entities(entity_type);
CREATE TABLE memory_audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id TEXT NOT NULL,
    action TEXT NOT NULL,
    caller_agent TEXT,
    timestamp TEXT NOT NULL,
    details TEXT
);
CREATE INDEX idx_audit_log_memory_id ON memory_audit_log(memory_id);
CREATE INDEX idx_audit_log_timestamp ON memory_audit_log(timestamp DESC);
CREATE TABLE memory_tombstones (
    memory_id TEXT PRIMARY KEY,
    provenance TEXT NOT NULL,
    access_policy TEXT NOT NULL,
    deleted_at TEXT NOT NULL,
    deleted_by_principal TEXT
);
CREATE INDEX idx_memory_tombstones_deleted_at ON memory_tombstones(deleted_at DESC);
CREATE TABLE scope_registry (
    scope_key TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    description TEXT,
    aliases TEXT NOT NULL DEFAULT '[]',
    matchers TEXT NOT NULL DEFAULT '[]',
    parent TEXT,
    related TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
);
CREATE TABLE memory_v2_metadata (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    scope_key TEXT,
    summary TEXT,
    agent_label TEXT,
    created_by_principal TEXT,
    quality_flags TEXT NOT NULL DEFAULT '[]',
    schema_version INTEGER NOT NULL DEFAULT 2,
    migrated_at TEXT,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_memory_v2_metadata_scope_key ON memory_v2_metadata(scope_key);

INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, has_embedding, embedding_revision, memory_type, importance, impression_count, activity_mass, updated_at, confidence)
VALUES ('01J00000000000000000000000', 'published fixture memory', '["upgrade"]', '{"source_agent":"fixture","source_conversation":"release","origin_conversation":"release"}', '{"type":"public"}', '2026-07-10T00:00:00Z', 1, 1, 'semantic', 0.75, 4, 1.25, '2026-07-10T00:00:01Z', 0.9);
INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, has_embedding, embedding_revision, memory_type, importance, impression_count, activity_mass, updated_at, confidence)
VALUES ('01J00000000000000000000002', 'published related memory', '["upgrade","relationship"]', '{"source_agent":"fixture-related","source_conversation":"release","origin_conversation":"release"}', '{"type":"public"}', '2026-07-10T00:00:10Z', 0, 0, 'semantic', 0.6, 1, 0.25, '2026-07-10T00:00:11Z', 0.85);
UPDATE memories SET superseded_by = '01J00000000000000000000002' WHERE id = '01J00000000000000000000000';
INSERT INTO memory_embeddings(rowid, embedding) VALUES (1, '[0.1,0.2,0.3]');
INSERT INTO memory_embedding_map(memory_id, vec_rowid) VALUES ('01J00000000000000000000000', 1);
INSERT INTO embedding_profile VALUES (1, 'openai-compatible', 'http://fixture.invalid/v1', 'fixture-model', 3);
INSERT INTO memory_entities VALUES ('01J00000000000000000000000', 'LocalHold', 'project');
INSERT INTO memory_audit_log(memory_id, action, caller_agent, timestamp, details) VALUES ('01J00000000000000000000000', 'store', 'fixture', '2026-07-10T00:00:02Z', '{"release":"beta"}');
INSERT INTO scope_registry VALUES ('org/gearbox', 'Gearbox', 'fixture parent scope', '["gearbox"]', '["gearbox/*"]', NULL, '["project/localhold"]', '2026-07-10T00:00:03Z');
INSERT INTO scope_registry VALUES ('project/localhold', 'LocalHold', 'fixture scope', '["localhold"]', '["*/localhold"]', 'org/gearbox', '["org/gearbox"]', '2026-07-10T00:00:03Z');
INSERT INTO memory_v2_metadata VALUES ('01J00000000000000000000000', 'project/localhold', 'fixture summary', 'fixture-agent', 'fixture-principal', '["fixture"]', 2, '2026-07-10T00:00:04Z', '2026-07-10T00:00:05Z');
INSERT INTO memory_tombstones VALUES ('01J00000000000000000000001', '{"source_agent":"fixture"}', '{"type":"public"}', '2026-07-10T00:00:06Z', 'fixture-principal');
