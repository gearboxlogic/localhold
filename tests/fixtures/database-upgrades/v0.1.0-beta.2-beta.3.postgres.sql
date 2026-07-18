CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE localhold_migrations (
    version BIGINT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE memories (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    tags JSONB NOT NULL,
    provenance JSONB NOT NULL,
    access_policy JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ,
    has_embedding BOOLEAN NOT NULL DEFAULT FALSE,
    embedding_revision BIGINT NOT NULL DEFAULT 0,
    memory_type TEXT NOT NULL DEFAULT 'semantic',
    importance DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    impression_count BIGINT NOT NULL DEFAULT 0,
    last_impressed_at TIMESTAMPTZ,
    superseded_by TEXT REFERENCES memories(id) ON DELETE SET NULL,
    activity_mass DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    last_used_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    confidence DOUBLE PRECISION NOT NULL DEFAULT 0.8,
    embedding_claimed_at TIMESTAMPTZ,
    embedding_claim_token TEXT
);
CREATE INDEX idx_memories_created_at ON memories(created_at DESC);
CREATE INDEX idx_memories_expires_at ON memories(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX idx_memories_has_embedding ON memories(has_embedding);
CREATE INDEX idx_memories_memory_type ON memories(memory_type);
CREATE INDEX idx_memories_superseded_by ON memories(superseded_by) WHERE superseded_by IS NOT NULL;
CREATE INDEX idx_memories_tags_gin ON memories USING GIN (tags);
CREATE INDEX idx_memories_source_agent ON memories ((provenance->>'source_agent'));
CREATE INDEX idx_memories_source_conversation ON memories ((provenance->>'source_conversation'));
CREATE INDEX idx_memories_origin_conversation ON memories ((provenance->>'origin_conversation'));
CREATE INDEX idx_memories_effective_origin_conversation ON memories (COALESCE(provenance->>'origin_conversation', provenance->>'source_conversation'));
CREATE INDEX idx_memories_access_type ON memories ((access_policy->>'type'));
CREATE INDEX idx_memories_content_fts ON memories USING GIN (to_tsvector('simple', content));
CREATE INDEX idx_memories_embedding_claim ON memories(has_embedding, embedding_claimed_at, created_at, id) WHERE has_embedding = FALSE;

CREATE TABLE memory_entities (
    memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    entity TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    PRIMARY KEY (memory_id, entity, entity_type)
);
CREATE INDEX idx_memory_entities_entity ON memory_entities(entity);
CREATE INDEX idx_memory_entities_entity_type ON memory_entities(entity_type);
CREATE TABLE memory_audit_log (
    id BIGSERIAL PRIMARY KEY,
    memory_id TEXT NOT NULL,
    action TEXT NOT NULL,
    caller_agent TEXT,
    timestamp TIMESTAMPTZ NOT NULL,
    details JSONB
);
CREATE INDEX idx_audit_log_memory_id ON memory_audit_log(memory_id);
CREATE INDEX idx_audit_log_timestamp ON memory_audit_log(timestamp DESC);
CREATE TABLE memory_tombstones (
    memory_id TEXT PRIMARY KEY,
    provenance JSONB NOT NULL,
    access_policy JSONB NOT NULL,
    deleted_at TIMESTAMPTZ NOT NULL,
    deleted_by_principal TEXT
);
CREATE INDEX idx_memory_tombstones_deleted_at ON memory_tombstones(deleted_at DESC);
CREATE TABLE scope_registry (
    scope_key TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    description TEXT,
    aliases JSONB NOT NULL DEFAULT '[]'::jsonb,
    matchers JSONB NOT NULL DEFAULT '[]'::jsonb,
    parent TEXT,
    related JSONB NOT NULL DEFAULT '[]'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE memory_v2_metadata (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    scope_key TEXT,
    summary TEXT,
    agent_label TEXT,
    created_by_principal TEXT,
    quality_flags JSONB NOT NULL DEFAULT '[]'::jsonb,
    schema_version BIGINT NOT NULL DEFAULT 2,
    migrated_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX idx_memory_v2_metadata_scope_key ON memory_v2_metadata(scope_key);
CREATE TABLE embedding_profile (
    singleton SMALLINT PRIMARY KEY CHECK (singleton = 1),
    provider TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    model TEXT NOT NULL,
    dimensions BIGINT NOT NULL CHECK (dimensions > 0)
);
CREATE TABLE memory_embeddings (
    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    embedding vector(3) NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO localhold_migrations(version, name) VALUES (1, 'bootstrap_schema'), (2, 'audit_log_without_memory_fk');
INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, has_embedding, embedding_revision, memory_type, importance, impression_count, activity_mass, updated_at, confidence)
VALUES ('01J00000000000000000000000', 'published fixture memory', '["upgrade"]', '{"source_agent":"fixture","source_conversation":"release","origin_conversation":"release"}', '{"type":"public"}', '2026-07-10T00:00:00Z', TRUE, 1, 'semantic', 0.75, 4, 1.25, '2026-07-10T00:00:01Z', 0.9);
INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, has_embedding, embedding_revision, memory_type, importance, impression_count, activity_mass, updated_at, confidence)
VALUES ('01J00000000000000000000002', 'published related memory', '["upgrade","relationship"]', '{"source_agent":"fixture-related","source_conversation":"release","origin_conversation":"release"}', '{"type":"public"}', '2026-07-10T00:00:10Z', FALSE, 0, 'semantic', 0.6, 1, 0.25, '2026-07-10T00:00:11Z', 0.85);
UPDATE memories SET superseded_by = '01J00000000000000000000002' WHERE id = '01J00000000000000000000000';
INSERT INTO memory_embeddings(memory_id, embedding) VALUES ('01J00000000000000000000000', '[0.1,0.2,0.3]');
INSERT INTO embedding_profile VALUES (1, 'openai-compatible', 'http://fixture.invalid/v1', 'fixture-model', 3);
INSERT INTO memory_entities VALUES ('01J00000000000000000000000', 'LocalHold', 'project');
INSERT INTO memory_audit_log(memory_id, action, caller_agent, timestamp, details) VALUES ('01J00000000000000000000000', 'store', 'fixture', '2026-07-10T00:00:02Z', '{"release":"beta"}');
INSERT INTO scope_registry VALUES ('org/gearbox', 'Gearbox', 'fixture parent scope', '["gearbox"]', '["gearbox/*"]', NULL, '["project/localhold"]', '2026-07-10T00:00:03Z');
INSERT INTO scope_registry VALUES ('project/localhold', 'LocalHold', 'fixture scope', '["localhold"]', '["*/localhold"]', 'org/gearbox', '["org/gearbox"]', '2026-07-10T00:00:03Z');
INSERT INTO memory_v2_metadata VALUES ('01J00000000000000000000000', 'project/localhold', 'fixture summary', 'fixture-agent', 'fixture-principal', '["fixture"]', 2, '2026-07-10T00:00:04Z', '2026-07-10T00:00:05Z');
INSERT INTO memory_tombstones VALUES ('01J00000000000000000000001', '{"source_agent":"fixture"}', '{"type":"public"}', '2026-07-10T00:00:06Z', 'fixture-principal');
