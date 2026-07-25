//! Backend-specific data migration helpers.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env::VarError,
    ffi::OsString,
    mem::size_of,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, types::ToSql};
use sqlx_core::{Error as SqlxError, query::query, query_scalar::query_scalar, row::Row as _, sql_str::AssertSqlSafe, transaction::Transaction, types::Json};
use sqlx_postgres::{PgConnection, PgPool, PgPoolOptions, PgRow, Postgres};

use super::{
    EmbeddingProfile, PostgresStore, SqliteStore,
    query::{MEMORY_COLUMN_COUNT, MEMORY_COLUMNS, row_to_memory},
    vector::validate_embedding_vector,
};
use crate::{
    config::PostgresDatabaseConfig,
    context::{ContextAuditEvent, ContextDefinition, ContextGrant, ContextId, ContextIdentity, ContextKind},
    error::{ParseEnumError, StoreError},
    types::{AccessPolicy, AuditAction, AuditEntry, Entity, Memory, MemoryId, MemoryMetadata, MemoryTombstone, Provenance},
};

const DEFAULT_BATCH_SIZE: usize = 500;
const DEFAULT_POSTGRES_URL_ENV: &str = "LOCALHOLD_POSTGRES_URL";
const SQLITE_EMBEDDING_FETCH_CHUNK_SIZE: usize = 500;
const POSTGRES_MIGRATIONS_TABLE: &str = "localhold_migrations";
const RETIRED_METADATA_TABLE: &str = "memory_v2_metadata";
const POSTGRES_LOCK_TIMEOUT: &str = "5s";
const POSTGRES_USER_TABLES: &[&str] = &[
    "memories",
    "memory_entities",
    "memory_embeddings",
    "memory_audit_log",
    "memory_tombstones",
    "context_kinds",
    "contexts",
    "context_aliases",
    "context_identities",
    "context_resolver_hints",
    "context_grants",
    "context_relations",
    "memory_contexts",
    "context_kind_policies",
    "context_anchor_overrides",
    "context_audit_events",
    "memory_metadata",
    "embedding_profile",
];
const POSTGRES_REQUIRED_COLUMNS: &[PostgresColumnExpectation] = &[
    PostgresColumnExpectation::new("localhold_migrations", "version", "bigint"),
    PostgresColumnExpectation::new("localhold_migrations", "name", "text"),
    PostgresColumnExpectation::new("localhold_migrations", "applied_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "id", "text"),
    PostgresColumnExpectation::new("memories", "content", "text"),
    PostgresColumnExpectation::new("memories", "tags", "jsonb"),
    PostgresColumnExpectation::new("memories", "provenance", "jsonb"),
    PostgresColumnExpectation::new("memories", "access_policy", "jsonb"),
    PostgresColumnExpectation::new("memories", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "expires_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "has_embedding", "boolean"),
    PostgresColumnExpectation::new("memories", "embedding_revision", "bigint"),
    PostgresColumnExpectation::new("memories", "record_revision", "bigint"),
    PostgresColumnExpectation::new("memories", "memory_type", "text"),
    PostgresColumnExpectation::new("memories", "importance", "double precision"),
    PostgresColumnExpectation::new("memories", "impression_count", "bigint"),
    PostgresColumnExpectation::new("memories", "last_impressed_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "superseded_by", "text"),
    PostgresColumnExpectation::new("memories", "activity_mass", "double precision"),
    PostgresColumnExpectation::new("memories", "last_used_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "confidence", "double precision"),
    PostgresColumnExpectation::new("memory_entities", "memory_id", "text"),
    PostgresColumnExpectation::new("memory_entities", "entity", "text"),
    PostgresColumnExpectation::new("memory_entities", "entity_type", "text"),
    PostgresColumnExpectation::new("memory_embeddings", "memory_id", "text"),
    PostgresColumnExpectation::new("memory_embeddings", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memory_audit_log", "id", "bigint"),
    PostgresColumnExpectation::new("memory_audit_log", "memory_id", "text"),
    PostgresColumnExpectation::new("memory_audit_log", "action", "text"),
    PostgresColumnExpectation::new("memory_audit_log", "caller_agent", "text"),
    PostgresColumnExpectation::new("memory_audit_log", "timestamp", "timestamp with time zone"),
    PostgresColumnExpectation::new("memory_audit_log", "details", "jsonb"),
    PostgresColumnExpectation::new("memory_tombstones", "memory_id", "text"),
    PostgresColumnExpectation::new("memory_tombstones", "provenance", "jsonb"),
    PostgresColumnExpectation::new("memory_tombstones", "access_policy", "jsonb"),
    PostgresColumnExpectation::new("memory_tombstones", "deleted_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memory_tombstones", "deleted_by_principal", "text"),
    PostgresColumnExpectation::new("context_kinds", "kind", "text"),
    PostgresColumnExpectation::new("context_kinds", "display_name", "text"),
    PostgresColumnExpectation::new("context_kinds", "builtin", "boolean"),
    PostgresColumnExpectation::new("context_kinds", "enabled", "boolean"),
    PostgresColumnExpectation::new("context_kinds", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_kinds", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("contexts", "id", "text"),
    PostgresColumnExpectation::new("contexts", "kind", "text"),
    PostgresColumnExpectation::new("contexts", "context_key", "text"),
    PostgresColumnExpectation::new("contexts", "normalized_key", "text"),
    PostgresColumnExpectation::new("contexts", "display_name", "text"),
    PostgresColumnExpectation::new("contexts", "description", "text"),
    PostgresColumnExpectation::new("contexts", "owner_principal", "text"),
    PostgresColumnExpectation::new("contexts", "guidance", "text"),
    PostgresColumnExpectation::new("contexts", "parent_id", "text"),
    PostgresColumnExpectation::new("contexts", "lifecycle", "text"),
    PostgresColumnExpectation::new("contexts", "frozen", "boolean"),
    PostgresColumnExpectation::new("contexts", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("contexts", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_aliases", "context_id", "text"),
    PostgresColumnExpectation::new("context_aliases", "alias", "text"),
    PostgresColumnExpectation::new("context_aliases", "normalized_alias", "text"),
    PostgresColumnExpectation::new("context_aliases", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_identities", "context_id", "text"),
    PostgresColumnExpectation::new("context_identities", "owner_principal", "text"),
    PostgresColumnExpectation::new("context_identities", "kind", "text"),
    PostgresColumnExpectation::new("context_identities", "scheme", "text"),
    PostgresColumnExpectation::new("context_identities", "namespace", "text"),
    PostgresColumnExpectation::new("context_identities", "fingerprint", "text"),
    PostgresColumnExpectation::new("context_identities", "redacted_label", "text"),
    PostgresColumnExpectation::new("context_identities", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_resolver_hints", "context_id", "text"),
    PostgresColumnExpectation::new("context_resolver_hints", "hint", "text"),
    PostgresColumnExpectation::new("context_resolver_hints", "normalized_hint", "text"),
    PostgresColumnExpectation::new("context_resolver_hints", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_grants", "context_id", "text"),
    PostgresColumnExpectation::new("context_grants", "grantee_principal", "text"),
    PostgresColumnExpectation::new("context_grants", "granted_by", "text"),
    PostgresColumnExpectation::new("context_grants", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_relations", "from_context_id", "text"),
    PostgresColumnExpectation::new("context_relations", "to_context_id", "text"),
    PostgresColumnExpectation::new("context_relations", "relation", "text"),
    PostgresColumnExpectation::new("context_relations", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memory_contexts", "memory_id", "text"),
    PostgresColumnExpectation::new("memory_contexts", "context_id", "text"),
    PostgresColumnExpectation::new("memory_contexts", "ordinal", "bigint"),
    PostgresColumnExpectation::new("memory_contexts", "created_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_kind_policies", "layer", "text"),
    PostgresColumnExpectation::new("context_kind_policies", "principal", "text"),
    PostgresColumnExpectation::new("context_kind_policies", "kind", "text"),
    PostgresColumnExpectation::new("context_kind_policies", "policy_json", "jsonb"),
    PostgresColumnExpectation::new("context_kind_policies", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_anchor_overrides", "anchor_context_id", "text"),
    PostgresColumnExpectation::new("context_anchor_overrides", "principal", "text"),
    PostgresColumnExpectation::new("context_anchor_overrides", "policy_json", "jsonb"),
    PostgresColumnExpectation::new("context_anchor_overrides", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_audit_events", "id", "bigint"),
    PostgresColumnExpectation::new("context_audit_events", "actor_principal", "text"),
    PostgresColumnExpectation::new("context_audit_events", "action", "text"),
    PostgresColumnExpectation::new("context_audit_events", "context_id", "text"),
    PostgresColumnExpectation::new("context_audit_events", "memory_id", "text"),
    PostgresColumnExpectation::new("context_audit_events", "timestamp", "timestamp with time zone"),
    PostgresColumnExpectation::new("context_audit_events", "details", "jsonb"),
    PostgresColumnExpectation::new("memory_metadata", "memory_id", "text"),
    PostgresColumnExpectation::new("memory_metadata", "scope_key", "text"),
    PostgresColumnExpectation::new("memory_metadata", "summary", "text"),
    PostgresColumnExpectation::new("memory_metadata", "agent_label", "text"),
    PostgresColumnExpectation::new("memory_metadata", "created_by_principal", "text"),
    PostgresColumnExpectation::new("memory_metadata", "quality_flags", "jsonb"),
    PostgresColumnExpectation::new("memory_metadata", "schema_version", "bigint"),
    PostgresColumnExpectation::new("memory_metadata", "migrated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memory_metadata", "updated_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("embedding_profile", "singleton", "smallint"),
    PostgresColumnExpectation::new("embedding_profile", "provider", "text"),
    PostgresColumnExpectation::new("embedding_profile", "endpoint", "text"),
    PostgresColumnExpectation::new("embedding_profile", "model", "text"),
    PostgresColumnExpectation::new("embedding_profile", "dimensions", "bigint"),
];
const POSTGRES_OPTIONAL_COLUMNS: &[PostgresColumnExpectation] = &[
    PostgresColumnExpectation::new("memories", "embedding_claimed_at", "timestamp with time zone"),
    PostgresColumnExpectation::new("memories", "embedding_claim_token", "text"),
];
const POSTGRES_NULLABLE_COLUMNS: &[(&str, &str)] = &[
    ("memories", "expires_at"),
    ("memories", "last_impressed_at"),
    ("memories", "superseded_by"),
    ("memories", "last_used_at"),
    ("memories", "embedding_claimed_at"),
    ("memories", "embedding_claim_token"),
    ("memory_audit_log", "caller_agent"),
    ("memory_audit_log", "details"),
    ("memory_tombstones", "deleted_by_principal"),
    ("contexts", "description"),
    ("contexts", "guidance"),
    ("contexts", "parent_id"),
    ("context_audit_events", "context_id"),
    ("context_audit_events", "memory_id"),
    ("context_audit_events", "details"),
    ("memory_metadata", "scope_key"),
    ("memory_metadata", "summary"),
    ("memory_metadata", "agent_label"),
    ("memory_metadata", "created_by_principal"),
    ("memory_metadata", "migrated_at"),
];
type PostgresDefaultExpectation = (&'static str, &'static str, &'static str);
const POSTGRES_REQUIRED_DEFAULTS: &[PostgresDefaultExpectation] = &[
    ("localhold_migrations", "applied_at", "now()"),
    ("memories", "has_embedding", "false"),
    ("memories", "record_revision", "0"),
    ("memories", "memory_type", "'semantic'::text"),
    ("memories", "importance", "0.5"),
    ("memories", "impression_count", "0"),
    ("memories", "activity_mass", "0.0"),
    ("memories", "updated_at", "now()"),
    ("memories", "confidence", "0.8"),
    ("memory_embeddings", "updated_at", "now()"),
    ("context_kinds", "builtin", "false"),
    ("context_kinds", "enabled", "true"),
    ("contexts", "lifecycle", "'active'::text"),
    ("contexts", "frozen", "false"),
    ("context_identities", "namespace", "''::text"),
    ("context_kind_policies", "principal", "''::text"),
    ("memory_metadata", "quality_flags", "'[]'::jsonb"),
    ("memory_metadata", "schema_version", "1"),
];
const POSTGRES_REQUIRED_KEYS: &[PostgresKeyExpectation] = &[
    PostgresKeyExpectation::new("localhold_migrations", &["version"]),
    PostgresKeyExpectation::new("localhold_migrations", &["name"]),
    PostgresKeyExpectation::new("memories", &["id"]),
    PostgresKeyExpectation::new("memory_entities", &["memory_id", "entity", "entity_type"]),
    PostgresKeyExpectation::new("memory_embeddings", &["memory_id"]),
    PostgresKeyExpectation::new("memory_audit_log", &["id"]),
    PostgresKeyExpectation::new("memory_tombstones", &["memory_id"]),
    PostgresKeyExpectation::new("context_kinds", &["kind"]),
    PostgresKeyExpectation::new("contexts", &["id"]),
    PostgresKeyExpectation::new("contexts", &["owner_principal", "kind", "normalized_key"]),
    PostgresKeyExpectation::new("context_aliases", &["context_id", "normalized_alias"]),
    PostgresKeyExpectation::new("context_identities", &["context_id", "scheme", "namespace", "fingerprint"]),
    PostgresKeyExpectation::new("context_identities", &["owner_principal", "kind", "scheme", "namespace", "fingerprint"]),
    PostgresKeyExpectation::new("context_resolver_hints", &["context_id", "normalized_hint"]),
    PostgresKeyExpectation::new("context_grants", &["context_id", "grantee_principal"]),
    PostgresKeyExpectation::new("context_relations", &["from_context_id", "to_context_id", "relation"]),
    PostgresKeyExpectation::new("memory_contexts", &["memory_id", "context_id"]),
    PostgresKeyExpectation::new("memory_contexts", &["memory_id", "ordinal"]),
    PostgresKeyExpectation::new("context_kind_policies", &["layer", "principal", "kind"]),
    PostgresKeyExpectation::new("context_anchor_overrides", &["anchor_context_id", "principal"]),
    PostgresKeyExpectation::new("context_audit_events", &["id"]),
    PostgresKeyExpectation::new("memory_metadata", &["memory_id"]),
    PostgresKeyExpectation::new("embedding_profile", &["singleton"]),
];
const POSTGRES_REQUIRED_FOREIGN_KEYS: &[PostgresForeignKeyExpectation] = &[
    PostgresForeignKeyExpectation::new("memories", "superseded_by", "memories", "id", "n"),
    PostgresForeignKeyExpectation::new("memory_entities", "memory_id", "memories", "id", "c"),
    PostgresForeignKeyExpectation::new("memory_embeddings", "memory_id", "memories", "id", "c"),
    PostgresForeignKeyExpectation::new("contexts", "kind", "context_kinds", "kind", "r"),
    PostgresForeignKeyExpectation::new("contexts", "parent_id", "contexts", "id", "r"),
    PostgresForeignKeyExpectation::new("context_aliases", "context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("context_identities", "context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("context_resolver_hints", "context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("context_grants", "context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("context_relations", "from_context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("context_relations", "to_context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("memory_contexts", "memory_id", "memories", "id", "c"),
    PostgresForeignKeyExpectation::new("memory_contexts", "context_id", "contexts", "id", "r"),
    PostgresForeignKeyExpectation::new("context_kind_policies", "kind", "context_kinds", "kind", "r"),
    PostgresForeignKeyExpectation::new("context_anchor_overrides", "anchor_context_id", "contexts", "id", "c"),
    PostgresForeignKeyExpectation::new("memory_metadata", "memory_id", "memories", "id", "c"),
];
const POSTGRES_REQUIRED_CHECKS: &[(&str, &str)] = &[
    ("contexts", "lifecycle = ANY (ARRAY['active'::text, 'archived'::text])"),
    ("contexts", "parent_id IS NULL OR parent_id <> id"),
    ("context_relations", "from_context_id <> to_context_id"),
    ("memory_contexts", "ordinal >= 0"),
    ("context_kind_policies", "layer = ANY (ARRAY['operator'::text, 'principal'::text])"),
    (
        "context_kind_policies",
        "layer = 'operator'::text AND principal = ''::text OR layer = 'principal'::text AND principal <> ''::text",
    ),
    ("embedding_profile", "singleton = 1"),
    ("embedding_profile", "dimensions > 0"),
];
const SQLITE_MEMORIES_COLUMNS: &[&str] = &[
    "id",
    "content",
    "tags",
    "provenance",
    "access_policy",
    "created_at",
    "expires_at",
    "has_embedding",
    "embedding_revision",
    "record_revision",
    "memory_type",
    "importance",
    "impression_count",
    "last_impressed_at",
    "superseded_by",
    "activity_mass",
    "last_used_at",
    "updated_at",
    "confidence",
    "embedding_claimed_at",
    "embedding_claim_token",
];
const SQLITE_EMBEDDING_MAP_COLUMNS: &[&str] = &["memory_id", "vec_rowid"];
const SQLITE_MEMORY_EMBEDDINGS_COLUMNS: &[&str] = &["embedding"];
const SQLITE_MEMORY_ENTITIES_COLUMNS: &[&str] = &["memory_id", "entity", "entity_type"];
const SQLITE_AUDIT_LOG_COLUMNS: &[&str] = &["id", "memory_id", "action", "caller_agent", "timestamp", "details"];
const SQLITE_TOMBSTONE_COLUMNS: &[&str] = &["memory_id", "provenance", "access_policy", "deleted_at", "deleted_by_principal"];
const SQLITE_SCOPE_REGISTRY_COLUMNS: &[&str] = &["scope_key", "display_name", "description", "aliases", "matchers", "parent", "related", "updated_at"];
const SQLITE_CONTEXT_KIND_COLUMNS: &[&str] = &["kind", "display_name", "builtin", "enabled", "created_at", "updated_at"];
const SQLITE_CONTEXT_COLUMNS: &[&str] = &[
    "id",
    "kind",
    "context_key",
    "normalized_key",
    "display_name",
    "description",
    "owner_principal",
    "guidance",
    "parent_id",
    "lifecycle",
    "frozen",
    "created_at",
    "updated_at",
];
const SQLITE_CONTEXT_ALIAS_COLUMNS: &[&str] = &["context_id", "alias", "normalized_alias", "created_at"];
const SQLITE_CONTEXT_IDENTITY_COLUMNS: &[&str] = &[
    "context_id",
    "owner_principal",
    "kind",
    "scheme",
    "namespace",
    "fingerprint",
    "redacted_label",
    "created_at",
];
const SQLITE_CONTEXT_HINT_COLUMNS: &[&str] = &["context_id", "hint", "normalized_hint", "created_at"];
const SQLITE_CONTEXT_GRANT_COLUMNS: &[&str] = &["context_id", "grantee_principal", "granted_by", "created_at"];
const SQLITE_CONTEXT_RELATION_COLUMNS: &[&str] = &["from_context_id", "to_context_id", "relation", "created_at"];
const SQLITE_MEMORY_CONTEXT_COLUMNS: &[&str] = &["memory_id", "context_id", "ordinal", "created_at"];
const SQLITE_CONTEXT_KIND_POLICY_COLUMNS: &[&str] = &["layer", "principal", "kind", "policy_json", "updated_at"];
const SQLITE_CONTEXT_ANCHOR_OVERRIDE_COLUMNS: &[&str] = &["anchor_context_id", "principal", "policy_json", "updated_at"];
const SQLITE_CONTEXT_AUDIT_COLUMNS: &[&str] = &["id", "actor_principal", "action", "context_id", "memory_id", "timestamp", "details"];
const SQLITE_METADATA_COLUMNS: &[&str] = &[
    "memory_id",
    "scope_key",
    "summary",
    "agent_label",
    "created_by_principal",
    "quality_flags",
    "schema_version",
    "migrated_at",
    "updated_at",
];
const SQLITE_EMBEDDING_PROFILE_COLUMNS: &[&str] = &["singleton", "provider", "endpoint", "model", "dimensions"];
const SQLITE_FTS_COLUMNS: &[&str] = &["content"];
const SQLITE_REQUIRED_TABLES: &[SqliteTableExpectation] = &[
    SqliteTableExpectation::new("memories", SQLITE_MEMORIES_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_embedding_map", SQLITE_EMBEDDING_MAP_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_embeddings", SQLITE_MEMORY_EMBEDDINGS_COLUMNS, &["using vec0", "float["]),
    SqliteTableExpectation::new("memory_entities", SQLITE_MEMORY_ENTITIES_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_fts", SQLITE_FTS_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_audit_log", SQLITE_AUDIT_LOG_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_tombstones", SQLITE_TOMBSTONE_COLUMNS, &[]),
    SqliteTableExpectation::new("context_kinds", SQLITE_CONTEXT_KIND_COLUMNS, &["check (builtin in (0, 1))", "check (enabled in (0, 1))"]),
    SqliteTableExpectation::new("contexts", SQLITE_CONTEXT_COLUMNS, &[
        "unique (owner_principal, kind, normalized_key)",
        "check (lifecycle in ('active', 'archived'))",
        "check (frozen in (0, 1))",
        "check (parent_id is null or parent_id <> id)",
    ]),
    SqliteTableExpectation::new("context_aliases", SQLITE_CONTEXT_ALIAS_COLUMNS, &[]),
    SqliteTableExpectation::new("context_identities", SQLITE_CONTEXT_IDENTITY_COLUMNS, &[
        "unique (owner_principal, kind, scheme, namespace, fingerprint)",
    ]),
    SqliteTableExpectation::new("context_resolver_hints", SQLITE_CONTEXT_HINT_COLUMNS, &[]),
    SqliteTableExpectation::new("context_grants", SQLITE_CONTEXT_GRANT_COLUMNS, &[]),
    SqliteTableExpectation::new("context_relations", SQLITE_CONTEXT_RELATION_COLUMNS, &["check (from_context_id <> to_context_id)"]),
    SqliteTableExpectation::new("memory_contexts", SQLITE_MEMORY_CONTEXT_COLUMNS, &["check (ordinal >= 0)", "unique (memory_id, ordinal)"]),
    SqliteTableExpectation::new("context_kind_policies", SQLITE_CONTEXT_KIND_POLICY_COLUMNS, &[
        "check (layer in ('operator', 'principal'))",
        "check ((layer = 'operator' and principal = '') or (layer = 'principal' and principal <> ''))",
    ]),
    SqliteTableExpectation::new("context_anchor_overrides", SQLITE_CONTEXT_ANCHOR_OVERRIDE_COLUMNS, &[]),
    SqliteTableExpectation::new("context_audit_events", SQLITE_CONTEXT_AUDIT_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_metadata", SQLITE_METADATA_COLUMNS, &[]),
    SqliteTableExpectation::new("embedding_profile", SQLITE_EMBEDDING_PROFILE_COLUMNS, &["check (singleton = 1)", "check (dimensions > 0)"]),
];
const SQLITE_REQUIRED_KEYS: &[SqliteKeyExpectation] = &[
    SqliteKeyExpectation::new("memories", &["id"]),
    SqliteKeyExpectation::new("memory_embedding_map", &["memory_id"]),
    SqliteKeyExpectation::new("memory_entities", &["memory_id", "entity", "entity_type"]),
    SqliteKeyExpectation::new("memory_audit_log", &["id"]),
    SqliteKeyExpectation::new("memory_tombstones", &["memory_id"]),
    SqliteKeyExpectation::new("context_kinds", &["kind"]),
    SqliteKeyExpectation::new("contexts", &["id"]),
    SqliteKeyExpectation::new("context_aliases", &["context_id", "normalized_alias"]),
    SqliteKeyExpectation::new("context_identities", &["context_id", "scheme", "namespace", "fingerprint"]),
    SqliteKeyExpectation::new("context_resolver_hints", &["context_id", "normalized_hint"]),
    SqliteKeyExpectation::new("context_grants", &["context_id", "grantee_principal"]),
    SqliteKeyExpectation::new("context_relations", &["from_context_id", "to_context_id", "relation"]),
    SqliteKeyExpectation::new("memory_contexts", &["memory_id", "context_id"]),
    SqliteKeyExpectation::new("context_kind_policies", &["layer", "principal", "kind"]),
    SqliteKeyExpectation::new("context_anchor_overrides", &["anchor_context_id", "principal"]),
    SqliteKeyExpectation::new("context_audit_events", &["id"]),
    SqliteKeyExpectation::new("memory_metadata", &["memory_id"]),
    SqliteKeyExpectation::new("embedding_profile", &["singleton"]),
];
const SQLITE_CONTEXT_UNIQUE_KEYS: &[SqliteUniqueExpectation] = &[
    SqliteUniqueExpectation::new("context_kinds", &[&["kind"]]),
    SqliteUniqueExpectation::new("contexts", &[&["id"], &["owner_principal", "kind", "normalized_key"]]),
    SqliteUniqueExpectation::new("context_aliases", &[&["context_id", "normalized_alias"]]),
    SqliteUniqueExpectation::new("context_identities", &[&["context_id", "scheme", "namespace", "fingerprint"], &[
        "owner_principal",
        "kind",
        "scheme",
        "namespace",
        "fingerprint",
    ]]),
    SqliteUniqueExpectation::new("context_resolver_hints", &[&["context_id", "normalized_hint"]]),
    SqliteUniqueExpectation::new("context_grants", &[&["context_id", "grantee_principal"]]),
    SqliteUniqueExpectation::new("context_relations", &[&["from_context_id", "to_context_id", "relation"]]),
    SqliteUniqueExpectation::new("memory_contexts", &[&["memory_id", "context_id"], &["memory_id", "ordinal"]]),
    SqliteUniqueExpectation::new("context_kind_policies", &[&["layer", "principal", "kind"]]),
    SqliteUniqueExpectation::new("context_anchor_overrides", &[&["anchor_context_id", "principal"]]),
    SqliteUniqueExpectation::new("context_audit_events", &[]),
];
const SQLITE_CONTEXT_FOREIGN_KEYS: &[SqliteForeignKeyExpectation] = &[
    SqliteForeignKeyExpectation::new("contexts", "kind", "context_kinds", "kind", "RESTRICT"),
    SqliteForeignKeyExpectation::new("contexts", "parent_id", "contexts", "id", "RESTRICT"),
    SqliteForeignKeyExpectation::new("context_aliases", "context_id", "contexts", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("context_identities", "context_id", "contexts", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("context_resolver_hints", "context_id", "contexts", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("context_grants", "context_id", "contexts", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("context_relations", "from_context_id", "contexts", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("context_relations", "to_context_id", "contexts", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("memory_contexts", "memory_id", "memories", "id", "CASCADE"),
    SqliteForeignKeyExpectation::new("memory_contexts", "context_id", "contexts", "id", "RESTRICT"),
    SqliteForeignKeyExpectation::new("context_kind_policies", "kind", "context_kinds", "kind", "RESTRICT"),
    SqliteForeignKeyExpectation::new("context_anchor_overrides", "anchor_context_id", "contexts", "id", "CASCADE"),
];
const SQLITE_REQUIRED_INDEXES: &[&str] = &[
    "idx_memories_created_at",
    "idx_memories_source_agent",
    "idx_memories_source_conversation",
    "idx_memories_origin_conversation",
    "idx_memories_effective_origin_conversation",
    "idx_memories_access_type",
    "idx_memories_expires_at",
    "idx_memories_has_embedding",
    "idx_memories_embedding_claim",
    "idx_memories_memory_type",
    "idx_memories_superseded_by",
    "idx_memory_entities_entity",
    "idx_memory_entities_entity_type",
    "idx_audit_log_memory_id",
    "idx_audit_log_timestamp",
    "idx_memory_tombstones_deleted_at",
    "idx_memory_metadata_scope_key",
    "idx_contexts_owner_kind_key",
    "idx_contexts_kind_key",
    "idx_contexts_key",
    "idx_contexts_parent",
    "idx_contexts_lifecycle",
    "idx_context_aliases_lookup",
    "idx_context_identities_lookup",
    "idx_context_identities_exact",
    "idx_context_resolver_hints_lookup",
    "idx_context_grants_principal",
    "idx_context_relations_reverse",
    "idx_memory_contexts_memory",
    "idx_memory_contexts_context",
    "idx_context_audit_context",
    "idx_context_audit_memory",
    "idx_context_audit_timestamp",
];
const SQLITE_V2_REQUIRED_TABLES: &[SqliteTableExpectation] = &[
    SqliteTableExpectation::new("memories", SQLITE_MEMORIES_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_embedding_map", SQLITE_EMBEDDING_MAP_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_embeddings", SQLITE_MEMORY_EMBEDDINGS_COLUMNS, &["using vec0", "float["]),
    SqliteTableExpectation::new("memory_entities", SQLITE_MEMORY_ENTITIES_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_fts", SQLITE_FTS_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_audit_log", SQLITE_AUDIT_LOG_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_tombstones", SQLITE_TOMBSTONE_COLUMNS, &[]),
    SqliteTableExpectation::new("scope_registry", SQLITE_SCOPE_REGISTRY_COLUMNS, &[]),
    SqliteTableExpectation::new("memory_metadata", SQLITE_METADATA_COLUMNS, &[]),
    SqliteTableExpectation::new("embedding_profile", SQLITE_EMBEDDING_PROFILE_COLUMNS, &["check (singleton = 1)", "check (dimensions > 0)"]),
];
const SQLITE_V2_REQUIRED_KEYS: &[SqliteKeyExpectation] = &[
    SqliteKeyExpectation::new("memories", &["id"]),
    SqliteKeyExpectation::new("memory_embedding_map", &["memory_id"]),
    SqliteKeyExpectation::new("memory_entities", &["memory_id", "entity", "entity_type"]),
    SqliteKeyExpectation::new("memory_audit_log", &["id"]),
    SqliteKeyExpectation::new("memory_tombstones", &["memory_id"]),
    SqliteKeyExpectation::new("scope_registry", &["scope_key"]),
    SqliteKeyExpectation::new("memory_metadata", &["memory_id"]),
    SqliteKeyExpectation::new("embedding_profile", &["singleton"]),
];
const SQLITE_V2_REQUIRED_INDEXES: &[&str] = &[
    "idx_memories_created_at",
    "idx_memories_source_agent",
    "idx_memories_source_conversation",
    "idx_memories_origin_conversation",
    "idx_memories_effective_origin_conversation",
    "idx_memories_access_type",
    "idx_memories_expires_at",
    "idx_memories_has_embedding",
    "idx_memories_embedding_claim",
    "idx_memories_memory_type",
    "idx_memories_superseded_by",
    "idx_memory_entities_entity",
    "idx_memory_entities_entity_type",
    "idx_audit_log_memory_id",
    "idx_audit_log_timestamp",
    "idx_memory_tombstones_deleted_at",
    "idx_memory_metadata_scope_key",
];
const SQLITE_REQUIRED_TRIGGERS: &[&str] = &[
    "trg_memory_embedding_map_delete",
    "trg_memory_clear_superseded_by",
    "trg_memory_fts_insert",
    "trg_memory_fts_update",
    "trg_memory_fts_delete",
];
pub(crate) const SQLITE_V1_SCHEMA_VERSION: u32 = 2;
const _: () = assert!(
    super::schema::SQLITE_SCHEMA_VERSION == SQLITE_V1_SCHEMA_VERSION + 1,
    "the previous-version restore upgrade contract must be revised when SQLite schema version changes"
);
const SQLITE_CURRENT_CLEAR_SUPERSEDED_TRIGGER: &str =
    "after delete on memories begin update memories set superseded_by = null, record_revision = record_revision + 1 where superseded_by = old.id; end";
const SQLITE_V1_CLEAR_SUPERSEDED_TRIGGER: &str = "after delete on memories begin update memories set superseded_by = null where superseded_by = old.id; end";

struct PostgresColumnExpectation {
    table: &'static str,
    column: &'static str,
    formatted_type: &'static str,
}

impl PostgresColumnExpectation {
    const fn new(table: &'static str, column: &'static str, formatted_type: &'static str) -> Self {
        Self { table, column, formatted_type }
    }
}

struct PostgresKeyExpectation {
    table: &'static str,
    columns: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct PostgresForeignKeyExpectation {
    child_table: &'static str,
    child_column: &'static str,
    parent_table: &'static str,
    parent_column: &'static str,
    delete_action: &'static str,
}

impl PostgresForeignKeyExpectation {
    const fn new(child_table: &'static str, child_column: &'static str, parent_table: &'static str, parent_column: &'static str, delete_action: &'static str) -> Self {
        Self {
            child_table,
            child_column,
            parent_table,
            parent_column,
            delete_action,
        }
    }
}

struct SqliteTableExpectation {
    name: &'static str,
    columns: &'static [&'static str],
    ddl_contains: &'static [&'static str],
}

impl SqliteTableExpectation {
    const fn new(name: &'static str, columns: &'static [&'static str], ddl_contains: &'static [&'static str]) -> Self {
        Self { name, columns, ddl_contains }
    }
}

struct SqliteKeyExpectation {
    table: &'static str,
    columns: &'static [&'static str],
}

struct SqliteUniqueExpectation {
    table: &'static str,
    keys: &'static [&'static [&'static str]],
}

impl SqliteUniqueExpectation {
    const fn new(table: &'static str, keys: &'static [&'static [&'static str]]) -> Self {
        Self { table, keys }
    }
}

struct SqliteForeignKeyExpectation {
    child_table: &'static str,
    child_column: &'static str,
    parent_table: &'static str,
    parent_column: &'static str,
    update_action: &'static str,
    delete_action: &'static str,
    match_clause: &'static str,
}

impl SqliteForeignKeyExpectation {
    const fn new(child_table: &'static str, child_column: &'static str, parent_table: &'static str, parent_column: &'static str, delete_action: &'static str) -> Self {
        Self {
            child_table,
            child_column,
            parent_table,
            parent_column,
            update_action: "NO ACTION",
            delete_action,
            match_clause: "NONE",
        }
    }
}

impl SqliteKeyExpectation {
    const fn new(table: &'static str, columns: &'static [&'static str]) -> Self {
        Self { table, columns }
    }
}

impl PostgresKeyExpectation {
    const fn new(table: &'static str, columns: &'static [&'static str]) -> Self {
        Self { table, columns }
    }
}

/// Options for migrating a SQLite database into an empty `PostgreSQL` database.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SqliteToPostgresOptions {
    /// Source SQLite database path.
    pub sqlite_path: PathBuf,
    /// Target `PostgreSQL` connection URL.
    pub postgres_url: String,
    /// Configured embedding dimensions.
    pub embedding_dimensions: usize,
    /// Number of memories processed per import chunk.
    pub batch_size: usize,
    /// Print the migration plan without writing to `PostgreSQL`.
    pub dry_run: bool,
    /// Required for actual imports.
    pub yes: bool,
}

impl std::fmt::Debug for SqliteToPostgresOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteToPostgresOptions")
            .field("sqlite_path", &self.sqlite_path)
            .field("postgres_url", &"[REDACTED]")
            .field("embedding_dimensions", &self.embedding_dimensions)
            .field("batch_size", &self.batch_size)
            .field("dry_run", &self.dry_run)
            .field("yes", &self.yes)
            .finish()
    }
}

impl SqliteToPostgresOptions {
    /// Parse arguments after `hold migrate sqlite-to-postgres`.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::Usage`] for missing or invalid arguments.
    pub fn parse_args(args: &[OsString]) -> Result<Self, MigrationError> {
        Self::parse_args_with_env(args, |name| std::env::var(name))
    }

    fn parse_args_with_env<F>(args: &[OsString], env_lookup: F) -> Result<Self, MigrationError>
    where
        F: Fn(&str) -> Result<String, VarError>,
    {
        let mut sqlite_path = None;
        let mut postgres_url = None;
        let mut postgres_url_env = None;
        let mut embedding_dimensions = None;
        let mut batch_size = DEFAULT_BATCH_SIZE;
        let mut dry_run = false;
        let mut yes = false;

        let mut idx = 0_usize;
        while idx < args.len() {
            let arg = args[idx].to_string_lossy();
            match arg.as_ref() {
                "--sqlite" => {
                    idx = idx.saturating_add(1);
                    sqlite_path = Some(PathBuf::from(next_arg(args, idx, "--sqlite")?));
                }
                "--postgres-url" => {
                    idx = idx.saturating_add(1);
                    postgres_url = Some(next_arg(args, idx, "--postgres-url")?.to_string_lossy().into_owned());
                }
                "--postgres-url-env" => {
                    idx = idx.saturating_add(1);
                    postgres_url_env = Some(parse_env_name_arg(args, idx, "--postgres-url-env")?);
                }
                "--embedding-dimensions" => {
                    idx = idx.saturating_add(1);
                    embedding_dimensions = Some(parse_usize_arg(args, idx, "--embedding-dimensions")?);
                }
                "--batch-size" => {
                    idx = idx.saturating_add(1);
                    batch_size = parse_usize_arg(args, idx, "--batch-size")?;
                }
                "--dry-run" => dry_run = true,
                "--yes" => yes = true,
                "-h" | "--help" => return Err(MigrationError::Usage(usage().into())),
                other => return Err(MigrationError::Usage(format!("unknown migration argument: {other}\n\n{}", usage()))),
            }
            idx = idx.saturating_add(1);
        }

        let sqlite_path = sqlite_path.ok_or_else(|| MigrationError::Usage(format!("missing --sqlite\n\n{}", usage())))?;
        let postgres_url = match postgres_url {
            Some(postgres_url) => postgres_url,
            None => postgres_url_from_env(postgres_url_env.as_deref().unwrap_or(DEFAULT_POSTGRES_URL_ENV), env_lookup)?,
        };
        let embedding_dimensions = embedding_dimensions.ok_or_else(|| MigrationError::Usage(format!("missing --embedding-dimensions\n\n{}", usage())))?;
        if embedding_dimensions == 0 {
            return Err(MigrationError::Usage("--embedding-dimensions must be greater than zero".into()));
        }
        if batch_size == 0 {
            return Err(MigrationError::Usage("--batch-size must be greater than zero".into()));
        }
        if dry_run && yes {
            return Err(MigrationError::Usage("--dry-run and --yes are mutually exclusive".into()));
        }

        Ok(Self {
            sqlite_path,
            postgres_url,
            embedding_dimensions,
            batch_size,
            dry_run,
            yes,
        })
    }
}

fn next_arg<'args>(args: &'args [OsString], idx: usize, name: &str) -> Result<&'args OsString, MigrationError> {
    args.get(idx)
        .filter(|value| !value.to_string_lossy().starts_with('-'))
        .ok_or_else(|| MigrationError::Usage(format!("{name} requires a value")))
}

fn parse_usize_arg(args: &[OsString], idx: usize, name: &str) -> Result<usize, MigrationError> {
    let value = next_arg(args, idx, name)?;
    value
        .to_string_lossy()
        .parse::<usize>()
        .map_err(|e| MigrationError::Usage(format!("{name} must be a positive integer: {e}")))
}

fn parse_env_name_arg(args: &[OsString], idx: usize, name: &str) -> Result<String, MigrationError> {
    let value = next_arg(args, idx, name)?.to_string_lossy().into_owned();
    if value.trim().is_empty() {
        return Err(MigrationError::Usage(format!("{name} requires a non-empty environment variable name")));
    }
    Ok(value)
}

fn postgres_url_from_env<F>(env_name: &str, env_lookup: F) -> Result<String, MigrationError>
where
    F: Fn(&str) -> Result<String, VarError>,
{
    match env_lookup(env_name) {
        Ok(value) if value.trim().is_empty() => Err(MigrationError::Usage(format!(
            "environment variable {env_name} is empty; set it or pass --postgres-url\n\n{}",
            usage()
        ))),
        Ok(value) => Ok(value),
        Err(VarError::NotPresent) => Err(MigrationError::Usage(format!(
            "missing --postgres-url and environment variable {env_name} is not set\n\n{}",
            usage()
        ))),
        Err(VarError::NotUnicode(_)) => Err(MigrationError::Usage(format!("environment variable {env_name} is not valid UTF-8"))),
    }
}

/// User-facing migration error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// CLI usage or safety-check failure.
    #[error("{0}")]
    Usage(String),
    /// Store-layer failure.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Count of rows in the user-data tables copied by the migration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationTableCounts {
    /// Memory rows.
    pub memories: u64,
    /// Entity rows.
    pub entities: u64,
    /// Embedding rows.
    pub embeddings: u64,
    /// Audit log rows.
    pub audit_entries: u64,
    /// Deleted-memory tombstone rows.
    pub tombstones: u64,
    /// Context-kind definition rows, including the four built-in kinds.
    pub context_kinds: u64,
    /// Context definition rows.
    pub contexts: u64,
    /// Context alias rows.
    pub context_aliases: u64,
    /// Durable context identity rows.
    pub context_identities: u64,
    /// Weak context resolver-hint rows.
    pub context_hints: u64,
    /// Context use-grant rows.
    pub context_grants: u64,
    /// Context relation rows.
    pub context_relations: u64,
    /// Direct memory-context membership rows.
    pub memory_contexts: u64,
    /// Context kind-policy rows.
    pub context_policies: u64,
    /// Context anchor-override rows.
    pub context_anchor_overrides: u64,
    /// Context audit-event rows.
    pub context_audit_events: u64,
    /// metadata rows.
    pub metadata: u64,
    /// Embedding vector-space profile rows (zero or one).
    pub embedding_profiles: u64,
}

impl MigrationTableCounts {
    const fn is_empty(self) -> bool {
        self.memories == 0
            && self.entities == 0
            && self.embeddings == 0
            && self.audit_entries == 0
            && self.tombstones == 0
            // A newly bootstrapped PostgreSQL target contains only these
            // built-ins. They are upserted from the source during import so
            // their timestamps and policy flags round-trip exactly.
            && self.context_kinds <= 4
            && self.contexts == 0
            && self.context_aliases == 0
            && self.context_identities == 0
            && self.context_hints == 0
            && self.context_grants == 0
            && self.context_relations == 0
            && self.memory_contexts == 0
            && self.context_policies == 0
            && self.context_anchor_overrides == 0
            && self.context_audit_events == 0
            && self.metadata == 0
            && self.embedding_profiles == 0
    }
}

/// Result summary for a SQLite-to-PostgreSQL migration run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationSummary {
    /// Source SQLite row counts.
    pub source: MigrationTableCounts,
    /// Target `PostgreSQL` row counts before import.
    pub target_before: MigrationTableCounts,
    /// Target `PostgreSQL` row counts after import. Dry runs leave this unset.
    pub target_after: Option<MigrationTableCounts>,
    /// Whether `PostgreSQL` was written.
    pub dry_run: bool,
}

impl MigrationSummary {
    /// Render a concise human-readable summary.
    #[must_use]
    pub fn render(&self) -> String {
        let mut output = String::new();
        output.push_str(if self.dry_run {
            "SQLite to PostgreSQL migration dry run\n"
        } else {
            "SQLite to PostgreSQL migration complete\n"
        });
        output.push_str("\nSource SQLite rows:\n");
        append_counts(&mut output, self.source);
        output.push_str("\nTarget PostgreSQL rows before import:\n");
        append_counts(&mut output, self.target_before);
        if let Some(target_after) = self.target_after {
            output.push_str("\nTarget PostgreSQL rows after import:\n");
            append_counts(&mut output, target_after);
        }
        output
    }
}

fn append_counts(output: &mut String, counts: MigrationTableCounts) {
    use std::fmt::Write as _;
    let _write_failed = writeln!(output, "  memories: {}", counts.memories).is_err();
    let _write_failed = writeln!(output, "  entities: {}", counts.entities).is_err();
    let _write_failed = writeln!(output, "  embeddings: {}", counts.embeddings).is_err();
    let _write_failed = writeln!(output, "  audit_entries: {}", counts.audit_entries).is_err();
    let _write_failed = writeln!(output, "  tombstones: {}", counts.tombstones).is_err();
    let _write_failed = writeln!(output, "  context_kinds: {}", counts.context_kinds).is_err();
    let _write_failed = writeln!(output, "  contexts: {}", counts.contexts).is_err();
    let _write_failed = writeln!(output, "  context_aliases: {}", counts.context_aliases).is_err();
    let _write_failed = writeln!(output, "  context_identities: {}", counts.context_identities).is_err();
    let _write_failed = writeln!(output, "  context_hints: {}", counts.context_hints).is_err();
    let _write_failed = writeln!(output, "  context_grants: {}", counts.context_grants).is_err();
    let _write_failed = writeln!(output, "  context_relations: {}", counts.context_relations).is_err();
    let _write_failed = writeln!(output, "  memory_contexts: {}", counts.memory_contexts).is_err();
    let _write_failed = writeln!(output, "  context_policies: {}", counts.context_policies).is_err();
    let _write_failed = writeln!(output, "  context_anchor_overrides: {}", counts.context_anchor_overrides).is_err();
    let _write_failed = writeln!(output, "  context_audit_events: {}", counts.context_audit_events).is_err();
    let _write_failed = writeln!(output, "  metadata: {}", counts.metadata).is_err();
    let _write_failed = writeln!(output, "  embedding_profiles: {}", counts.embedding_profiles).is_err();
}

/// Usage text for the migration subcommand.
#[must_use]
pub const fn usage() -> &'static str {
    "Usage: hold migrate sqlite-to-postgres --sqlite PATH [--postgres-url URL | --postgres-url-env NAME] --embedding-dimensions N [--batch-size N] [--dry-run | --yes]\n       If no PostgreSQL URL option is supplied, LOCALHOLD_POSTGRES_URL is used."
}

/// Migrate a SQLite source database into an empty `PostgreSQL` target.
///
/// # Errors
///
/// Returns an error when the source is invalid, the target is non-empty, or an
/// import/verification step fails.
pub async fn migrate_sqlite_to_postgres(options: &SqliteToPostgresOptions) -> Result<MigrationSummary, MigrationError> {
    if !options.dry_run && !options.yes {
        return Err(MigrationError::Usage("actual migration requires --yes".into()));
    }
    if !options.sqlite_path.exists() {
        return Err(StoreError::NotFound(format!("SQLite source not found: {}", options.sqlite_path.display())).into());
    }

    let source = export_sqlite(options).await?;
    validate_supersession_links(&source)?;

    if options.dry_run {
        let target_before = preflight_empty_postgres_target(options).await?;
        return Ok(MigrationSummary {
            source: source.counts,
            target_before,
            target_after: None,
            dry_run: true,
        });
    }

    let _target_before_preflight = preflight_empty_postgres_target(options).await?;
    let postgres_config = PostgresDatabaseConfig {
        url: options.postgres_url.clone(),
        auto_migrate: true,
        ..PostgresDatabaseConfig::default()
    };
    let target = PostgresStore::open(&postgres_config, options.embedding_dimensions).await?;
    let mut tx = target.pool().begin().await.map_err(StoreError::from)?;
    lock_postgres_user_tables(&mut tx).await?;
    let target_before = postgres_counts_tx(&mut tx).await?;
    if !target_before.is_empty() {
        return Err(StoreError::Conflict(format!("PostgreSQL target is not empty: {target_before:?}")).into());
    }

    import_postgres(&mut tx, &source, options.batch_size).await?;
    let target_after = postgres_counts_tx(&mut tx).await?;
    verify_counts(source.counts, target_after)?;
    verify_migrated_values(&source, &mut tx, options.embedding_dimensions).await?;
    tx.commit().await.map_err(StoreError::from)?;

    Ok(MigrationSummary {
        source: source.counts,
        target_before,
        target_after: Some(target_after),
        dry_run: false,
    })
}

async fn preflight_empty_postgres_target(options: &SqliteToPostgresOptions) -> Result<MigrationTableCounts, StoreError> {
    let target_pool = open_postgres_pool(&options.postgres_url).await?;
    validate_existing_postgres_schema(&target_pool, options.embedding_dimensions, true, true).await?;
    check_existing_postgres_vector_dimensions(&target_pool, options.embedding_dimensions).await?;
    let target_before = postgres_counts_existing(&target_pool).await?;
    if !target_before.is_empty() {
        return Err(StoreError::Conflict(format!("PostgreSQL target is not empty: {target_before:?}")));
    }
    Ok(target_before)
}

#[derive(Clone, Debug)]
struct MigrationSnapshot {
    memories: Vec<MigrationMemory>,
    superseded_links: Vec<(MemoryId, MemoryId)>,
    audit_entries: Vec<MigrationAuditEntry>,
    tombstones: Vec<MigrationTombstone>,
    contexts: MigrationContextData,
    metadata: Vec<MigrationMetadata>,
    embedding_profile: Option<EmbeddingProfile>,
    counts: MigrationTableCounts,
}

#[derive(Clone, Debug)]
struct MigrationMemory {
    memory: Memory,
    embedding_revision: i64,
    embedding: Option<Vec<f32>>,
}

#[derive(Clone, Debug)]
struct MigrationAuditEntry {
    id: i64,
    memory_id: MemoryId,
    entry: AuditEntry,
}

#[derive(Clone, Debug)]
struct MigrationTombstone {
    tombstone: MemoryTombstone,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextKind {
    kind: String,
    display_name: String,
    builtin: bool,
    enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContext {
    definition: ContextDefinition,
    normalized_key: String,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextAlias {
    context_id: ContextId,
    alias: String,
    normalized_alias: String,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextIdentity {
    context_id: ContextId,
    owner_principal: String,
    kind: String,
    identity: ContextIdentity,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextHint {
    context_id: ContextId,
    hint: String,
    normalized_hint: String,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextRelation {
    from_context_id: ContextId,
    to_context_id: ContextId,
    relation: String,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationMemoryContext {
    memory_id: MemoryId,
    context_id: ContextId,
    ordinal: i64,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextPolicy {
    layer: String,
    principal: String,
    kind: String,
    policy: serde_json::Value,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MigrationContextAnchorOverride {
    anchor_context_id: ContextId,
    principal: String,
    policy: serde_json::Value,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
struct MigrationContextData {
    kinds: Vec<MigrationContextKind>,
    contexts: Vec<MigrationContext>,
    aliases: Vec<MigrationContextAlias>,
    identities: Vec<MigrationContextIdentity>,
    hints: Vec<MigrationContextHint>,
    grants: Vec<ContextGrant>,
    relations: Vec<MigrationContextRelation>,
    memberships: Vec<MigrationMemoryContext>,
    policies: Vec<MigrationContextPolicy>,
    anchor_overrides: Vec<MigrationContextAnchorOverride>,
    audit_events: Vec<ContextAuditEvent>,
}

#[derive(Clone, Debug)]
struct MigrationMetadata {
    metadata: MemoryMetadata,
    migrated_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

async fn export_sqlite(options: &SqliteToPostgresOptions) -> Result<MigrationSnapshot, StoreError> {
    let sqlite_path = options.sqlite_path.clone();
    let embedding_dimensions = options.embedding_dimensions;
    tokio::task::spawn_blocking(move || export_sqlite_path(&sqlite_path, embedding_dimensions))
        .await
        .map_err(|e| StoreError::Database(Box::new(e)))?
}

fn export_sqlite_path(path: &Path, embedding_dimensions: usize) -> Result<MigrationSnapshot, StoreError> {
    SqliteStore::register_extension()?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let tx = conn.unchecked_transaction()?;
    let snapshot = export_sqlite_conn(&tx, embedding_dimensions)?;
    tx.commit()?;
    Ok(snapshot)
}

fn export_sqlite_conn(conn: &Connection, embedding_dimensions: usize) -> Result<MigrationSnapshot, StoreError> {
    validate_sqlite_source_schema(conn, embedding_dimensions)?;
    let mut stmt = conn.prepare(&format!("SELECT {MEMORY_COLUMNS}, embedding_revision FROM memories ORDER BY created_at ASC, id ASC"))?;
    let mut memories = stmt
        .query_and_then([], |row| {
            let memory = row_to_memory(row)?;
            let embedding_revision = row.get(MEMORY_COLUMN_COUNT)?;
            Ok((memory, embedding_revision))
        })?
        .collect::<Result<Vec<_>, StoreError>>()?;
    let ids: Vec<MemoryId> = memories.iter().map(|(memory, _)| memory.id).collect();
    let entity_map = super::crud::hydrate_entities_batch(conn, &ids)?;
    for (memory, _) in &mut memories {
        memory.entities = entity_map.get(&memory.id).cloned().unwrap_or_default();
    }

    let mut embeddings = super::EmbeddingMap::new();
    for chunk in ids.chunks(SQLITE_EMBEDDING_FETCH_CHUNK_SIZE) {
        embeddings.extend(fetch_sqlite_embeddings_strict(conn, chunk, embedding_dimensions)?);
    }

    let mut migration_memories = Vec::with_capacity(memories.len());
    let mut superseded_links = Vec::new();
    for (mut memory, embedding_revision) in memories {
        let embedding = embeddings.remove(&memory.id);
        if memory.has_embedding && embedding.is_none() {
            return Err(StoreError::Conflict(format!("memory {} has has_embedding=true but no valid embedding vector", memory.id)));
        }
        if !memory.has_embedding && embedding.is_some() {
            return Err(StoreError::Conflict(format!(
                "memory {} has has_embedding=false but still has an embedding vector",
                memory.id
            )));
        }
        if let Some(superseded_by) = memory.superseded_by.take() {
            superseded_links.push((memory.id, superseded_by));
        }
        migration_memories.push(MigrationMemory {
            memory,
            embedding_revision,
            embedding,
        });
    }

    let audit_entries = export_audit_entries(conn)?;
    let tombstones = export_tombstones(conn)?;
    let contexts = export_sqlite_contexts(conn)?;
    let metadata = export_metadata(conn)?;
    let counts = sqlite_counts(conn)?;

    Ok(MigrationSnapshot {
        memories: migration_memories,
        superseded_links,
        audit_entries,
        tombstones,
        contexts,
        metadata,
        embedding_profile: export_sqlite_embedding_profile(conn)?,
        counts,
    })
}

pub(crate) fn validate_sqlite_source_schema(conn: &Connection, embedding_dimensions: usize) -> Result<(), StoreError> {
    let schema_version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version > super::schema::SQLITE_SCHEMA_VERSION {
        return Err(sqlite_newer_source_schema_error(schema_version));
    }
    if schema_version != super::schema::SQLITE_SCHEMA_VERSION {
        return Err(sqlite_source_schema_error(format!(
            "schema version is {schema_version}, expected {}",
            super::schema::SQLITE_SCHEMA_VERSION
        )));
    }
    reject_retired_sqlite_schema(conn)?;
    if sqlite_schema_sql(conn, "table", "scope_registry")?.is_some() {
        return Err(sqlite_source_schema_error("current SQLite schema contains retired scope_registry"));
    }
    for table in SQLITE_REQUIRED_TABLES {
        validate_sqlite_table(conn, table)?;
    }
    for key in SQLITE_REQUIRED_KEYS {
        validate_sqlite_primary_key(conn, key)?;
    }
    validate_sqlite_context_unique_keys(conn)?;
    validate_sqlite_context_foreign_keys(conn)?;
    for index in SQLITE_REQUIRED_INDEXES {
        validate_sqlite_schema_object_exists(conn, "index", index)?;
    }
    for trigger in SQLITE_REQUIRED_TRIGGERS {
        validate_sqlite_schema_object_exists(conn, "trigger", trigger)?;
    }
    validate_sqlite_managed_object_definitions(conn, false, true, SQLITE_CURRENT_CLEAR_SUPERSEDED_TRIGGER)?;
    super::schema::check_dimension_mismatch(conn, embedding_dimensions)?;
    validate_sqlite_foreign_key_integrity(conn)?;
    validate_embedding_map_integrity(conn)?;
    validate_sqlite_context_integrity(conn)?;
    Ok(())
}

/// Validate the immediately previous SQLite contract before upgrading a private restore stage.
pub(crate) fn validate_sqlite_v1_source_schema_for_upgrade(conn: &Connection, embedding_dimensions: usize) -> Result<(), StoreError> {
    let schema_version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version != SQLITE_V1_SCHEMA_VERSION {
        return Err(sqlite_source_schema_error(format!(
            "schema version is {schema_version}, expected supported upgrade source {SQLITE_V1_SCHEMA_VERSION}"
        )));
    }
    reject_retired_sqlite_schema(conn)?;
    for table in SQLITE_V2_REQUIRED_TABLES {
        validate_sqlite_table_allowing_missing(conn, table, &[])?;
    }
    for key in SQLITE_V2_REQUIRED_KEYS {
        validate_sqlite_primary_key(conn, key)?;
    }
    for index in SQLITE_V2_REQUIRED_INDEXES {
        validate_sqlite_schema_object_exists(conn, "index", index)?;
    }
    for trigger in SQLITE_REQUIRED_TRIGGERS {
        validate_sqlite_schema_object_exists(conn, "trigger", trigger)?;
    }
    validate_sqlite_managed_object_definitions(conn, false, false, SQLITE_CURRENT_CLEAR_SUPERSEDED_TRIGGER)?;
    super::schema::check_dimension_mismatch(conn, embedding_dimensions)?;
    validate_sqlite_foreign_key_integrity(conn)?;
    validate_embedding_map_integrity(conn)?;
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "managed SQLite object definitions are validated from one auditable manifest")]
#[expect(clippy::excessive_nesting, reason = "optional migration-time objects require nested absent/conflict handling")]
fn validate_sqlite_managed_object_definitions(conn: &Connection, allow_missing: bool, include_contexts: bool, clear_superseded_definition: &'static str) -> Result<(), StoreError> {
    const INDEX_DEFINITIONS: &[(&str, &str)] = &[
        ("idx_memories_created_at", "on memories(created_at desc)"),
        ("idx_memories_source_agent", "on memories(json_extract(provenance, '$.source_agent'))"),
        ("idx_memories_source_conversation", "on memories(json_extract(provenance, '$.source_conversation'))"),
        ("idx_memories_origin_conversation", "on memories(json_extract(provenance, '$.origin_conversation'))"),
        (
            "idx_memories_effective_origin_conversation",
            "on memories(coalesce(json_extract(provenance, '$.origin_conversation'), json_extract(provenance, '$.source_conversation')))",
        ),
        ("idx_memories_access_type", "on memories(json_extract(access_policy, '$.type'))"),
        ("idx_memories_expires_at", "on memories(expires_at) where expires_at is not null"),
        ("idx_memories_has_embedding", "on memories(has_embedding)"),
        (
            "idx_memories_embedding_claim",
            "on memories(has_embedding, embedding_claimed_at, created_at, id) where has_embedding = 0",
        ),
        ("idx_memories_memory_type", "on memories(memory_type)"),
        ("idx_memories_superseded_by", "on memories(superseded_by) where superseded_by is not null"),
        ("idx_memory_entities_entity", "on memory_entities(entity)"),
        ("idx_memory_entities_entity_type", "on memory_entities(entity_type)"),
        ("idx_audit_log_memory_id", "on memory_audit_log(memory_id)"),
        ("idx_audit_log_timestamp", "on memory_audit_log(timestamp desc)"),
        ("idx_memory_metadata_scope_key", "on memory_metadata(scope_key)"),
        ("idx_memory_tombstones_deleted_at", "on memory_tombstones(deleted_at desc)"),
    ];
    const CONTEXT_INDEX_DEFINITIONS: &[(&str, &str)] = &[
        ("idx_contexts_owner_kind_key", "on contexts(owner_principal, kind, normalized_key)"),
        ("idx_contexts_kind_key", "on contexts(kind, normalized_key, id)"),
        ("idx_contexts_key", "on contexts(normalized_key, id)"),
        ("idx_contexts_parent", "on contexts(parent_id) where parent_id is not null"),
        ("idx_contexts_lifecycle", "on contexts(lifecycle, kind)"),
        ("idx_context_aliases_lookup", "on context_aliases(normalized_alias, context_id)"),
        (
            "idx_context_identities_lookup",
            "on context_identities(owner_principal, kind, scheme, namespace, fingerprint)",
        ),
        ("idx_context_identities_exact", "on context_identities(kind, scheme, namespace, fingerprint, context_id)"),
        ("idx_context_resolver_hints_lookup", "on context_resolver_hints(normalized_hint, context_id)"),
        ("idx_context_grants_principal", "on context_grants(grantee_principal, context_id)"),
        ("idx_context_relations_reverse", "on context_relations(to_context_id, relation, from_context_id)"),
        ("idx_memory_contexts_memory", "on memory_contexts(memory_id, ordinal, context_id)"),
        ("idx_memory_contexts_context", "on memory_contexts(context_id, memory_id)"),
        (
            "idx_context_audit_context",
            "on context_audit_events(context_id, timestamp desc) where context_id is not null",
        ),
        ("idx_context_audit_memory", "on context_audit_events(memory_id, timestamp desc) where memory_id is not null"),
        ("idx_context_audit_timestamp", "on context_audit_events(timestamp desc)"),
    ];
    let trigger_definitions = [
        (
            "trg_memory_embedding_map_delete",
            "after delete on memory_embedding_map begin delete from memory_embeddings where rowid = old.vec_rowid; end",
        ),
        ("trg_memory_clear_superseded_by", clear_superseded_definition),
        (
            "trg_memory_fts_insert",
            "after insert on memories begin insert into memory_fts(rowid, content) values (new.rowid, new.content); end",
        ),
        (
            "trg_memory_fts_update",
            "after update of content on memories begin insert into memory_fts(memory_fts, rowid, content) values('delete', old.rowid, old.content); insert into memory_fts(rowid, content) values (new.rowid, new.content); end",
        ),
        (
            "trg_memory_fts_delete",
            "before delete on memories begin insert into memory_fts(memory_fts, rowid, content) values('delete', old.rowid, old.content); end",
        ),
    ];
    for (name, expected) in INDEX_DEFINITIONS {
        let Some(sql) = normalized_sqlite_schema_sql(conn, "index", name)? else {
            if allow_missing {
                reject_conflicting_sqlite_schema_object(conn, "index", name)?;
                continue;
            }
            return Err(sqlite_source_schema_error(format!("required index {name} is missing")));
        };
        if sql != format!("create index {name} {expected}") {
            return Err(sqlite_source_schema_error(format!("required index {name} has an incompatible definition")));
        }
    }
    if include_contexts {
        for (name, expected) in CONTEXT_INDEX_DEFINITIONS {
            let Some(sql) = normalized_sqlite_schema_sql(conn, "index", name)? else {
                if allow_missing {
                    reject_conflicting_sqlite_schema_object(conn, "index", name)?;
                    continue;
                }
                return Err(sqlite_source_schema_error(format!("required index {name} is missing")));
            };
            if sql != format!("create index {name} {expected}") {
                return Err(sqlite_source_schema_error(format!("required index {name} has an incompatible definition")));
            }
        }
    }
    for (name, expected) in trigger_definitions {
        let Some(sql) = normalized_sqlite_schema_sql(conn, "trigger", name)? else {
            if allow_missing {
                continue;
            }
            return Err(sqlite_source_schema_error(format!("required trigger {name} is missing")));
        };
        if sql != format!("create trigger {name} {expected}") {
            return Err(sqlite_source_schema_error(format!("required trigger {name} has an incompatible definition")));
        }
    }
    Ok(())
}

fn reject_conflicting_sqlite_schema_object(conn: &Connection, expected_type: &'static str, name: &'static str) -> Result<(), StoreError> {
    let actual_type: Option<String> = conn
        .query_row("SELECT type FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view', 'index')", [name], |row| {
            row.get(0)
        })
        .optional()?;
    if let Some(actual_type) = actual_type {
        return Err(sqlite_source_schema_error(format!(
            "managed SQLite {expected_type} name {name} is occupied by a {actual_type}"
        )));
    }
    Ok(())
}

fn normalized_sqlite_schema_sql(conn: &Connection, object_type: &'static str, name: &'static str) -> Result<Option<String>, StoreError> {
    Ok(sqlite_schema_sql(conn, object_type, name)?.map(|sql| {
        sql.to_ascii_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace("create index if not exists ", "create index ")
            .replace("create trigger if not exists ", "create trigger ")
            .trim_end_matches(';')
            .to_owned()
    }))
}

/// Validate every managed SQLite table that is already present without
/// requiring tables or columns that a supported startup migration may add.
pub(crate) fn validate_present_sqlite_schema(conn: &Connection) -> Result<(), StoreError> {
    validate_present_sqlite_schema_inner(conn, false)
}

/// Validate all present managed objects while allowing the separately
/// validated metadata table written by published beta releases.
pub(crate) fn validate_present_sqlite_schema_for_published_upgrade(conn: &Connection) -> Result<(), StoreError> {
    validate_present_sqlite_schema_inner(conn, true)
}

#[expect(
    clippy::too_many_lines,
    reason = "present-schema validation keeps the complete pre-migration SQLite contract in one reviewable pass"
)]
fn validate_present_sqlite_schema_inner(conn: &Connection, allow_published_metadata: bool) -> Result<(), StoreError> {
    const MIGRATABLE_MEMORY_COLUMNS: &[&str] = &[
        "embedding_revision",
        "record_revision",
        "memory_type",
        "importance",
        "impression_count",
        "last_impressed_at",
        "superseded_by",
        "activity_mass",
        "last_used_at",
        "updated_at",
        "confidence",
        "embedding_claimed_at",
        "embedding_claim_token",
    ];
    let schema_version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema_version > super::schema::SQLITE_SCHEMA_VERSION {
        return Err(sqlite_newer_source_schema_error(schema_version));
    }
    if !allow_published_metadata {
        reject_retired_sqlite_schema(conn)?;
    }
    for table in SQLITE_REQUIRED_TABLES {
        if sqlite_schema_sql(conn, "table", table.name)?.is_none() {
            validate_absent_sqlite_table_conflicts(conn, table.name)?;
            continue;
        }
        let raw_sql = sqlite_schema_sql(conn, "table", table.name)?.unwrap_or_default();
        let sql = normalize_sqlite_ddl(&sqlite_sql_without_comments(&raw_sql));
        for fragment in table.ddl_contains {
            if !fragment.trim_start().to_ascii_lowercase().starts_with("check") && !sql.contains(&normalize_sqlite_ddl(fragment)) {
                return Err(sqlite_source_schema_error(format!(
                    "table {} has unexpected DDL; expected declaration containing {fragment:?}",
                    table.name
                )));
            }
        }
        validate_sqlite_check_constraints(table, &raw_sql)?;
        if table.name == "memory_fts" {
            validate_sqlite_fts_external_content(&sql)?;
        }
        let columns = sqlite_table_columns(conn, table.name)?;
        validate_published_sqlite_record_revision(table.name, &columns, allow_published_metadata)?;
        for column in table.columns {
            if table.name == "memories" && MIGRATABLE_MEMORY_COLUMNS.contains(column) {
                continue;
            }
            if !columns.contains(*column) {
                return Err(sqlite_source_schema_error(format!("table {} is missing required column {column}", table.name)));
            }
        }
        if table.name == "memories" {
            validate_sqlite_embedding_revision_contract(conn)?;
        }
    }
    for key in SQLITE_REQUIRED_KEYS {
        if sqlite_schema_sql(conn, "table", key.table)?.is_some() {
            validate_sqlite_primary_key(conn, key)?;
        }
    }
    if sqlite_schema_sql(conn, "table", "memories")?.is_some() {
        let columns = sqlite_table_columns(conn, "memories")?;
        let old_pair = (columns.contains("access_count"), columns.contains("last_accessed_at"));
        let current_pair = (columns.contains("impression_count"), columns.contains("last_impressed_at"));
        if !matches!((old_pair, current_pair), ((false, false), (false, false) | (true, true)) | ((true, true), (false, false))) {
            return Err(sqlite_source_schema_error(
                "memories impression tracking columns are in a mixed state that startup cannot migrate",
            ));
        }
    }
    let clear_superseded_trigger = if allow_published_metadata {
        SQLITE_V1_CLEAR_SUPERSEDED_TRIGGER
    } else {
        SQLITE_CURRENT_CLEAR_SUPERSEDED_TRIGGER
    };
    validate_sqlite_managed_object_definitions(conn, true, schema_version >= super::schema::SQLITE_SCHEMA_VERSION, clear_superseded_trigger)?;
    Ok(())
}

fn validate_published_sqlite_record_revision(table: &str, columns: &HashSet<String>, allow_published_metadata: bool) -> Result<(), StoreError> {
    if allow_published_metadata && table == "memories" && columns.contains("record_revision") {
        return Err(sqlite_source_schema_error(
            "published-beta memories table unexpectedly contains record_revision; startup only supports adding the canonical column",
        ));
    }
    Ok(())
}

fn validate_absent_sqlite_table_conflicts(conn: &Connection, table: &'static str) -> Result<(), StoreError> {
    if table == "memory_fts" {
        let shadow_conflict: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('memory_fts_data', 'memory_fts_idx', 'memory_fts_docsize', 'memory_fts_config') AND type IN ('table', 'view', 'index'))",
            [],
            |row| row.get(0),
        )?;
        if shadow_conflict {
            return Err(sqlite_source_schema_error("memory_fts is absent but a reserved FTS5 shadow-table name is occupied"));
        }
    }
    let conflicting_object: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1 AND type IN ('view', 'index'))", [table], |row| {
        row.get(0)
    })?;
    if conflicting_object {
        return Err(sqlite_source_schema_error(format!("managed SQLite object {table} exists but is not a table")));
    }
    Ok(())
}

fn validate_sqlite_table(conn: &Connection, expectation: &SqliteTableExpectation) -> Result<(), StoreError> {
    validate_sqlite_table_allowing_missing(conn, expectation, &[])
}

fn validate_sqlite_table_allowing_missing(conn: &Connection, expectation: &SqliteTableExpectation, allowed_missing_columns: &[&str]) -> Result<(), StoreError> {
    let sql = sqlite_schema_sql(conn, "table", expectation.name)?.ok_or_else(|| sqlite_source_schema_error(format!("required table {} is missing", expectation.name)))?;
    let normalized_sql = normalize_sqlite_ddl(&sqlite_sql_without_comments(&sql));
    for fragment in expectation.ddl_contains {
        if !fragment.trim_start().to_ascii_lowercase().starts_with("check") && !normalized_sql.contains(&normalize_sqlite_ddl(fragment)) {
            return Err(sqlite_source_schema_error(format!(
                "table {} has unexpected DDL; expected declaration containing {fragment:?}",
                expectation.name
            )));
        }
    }
    validate_sqlite_check_constraints(expectation, &sql)?;
    if expectation.name == "memory_fts" {
        validate_sqlite_fts_external_content(&sql)?;
    }

    let columns = sqlite_table_columns(conn, expectation.name)?;
    for column in expectation.columns {
        if allowed_missing_columns.contains(column) {
            continue;
        }
        if !columns.contains(*column) {
            return Err(sqlite_source_schema_error(format!("table {} is missing required column {column}", expectation.name)));
        }
    }
    if expectation.name == "memories" {
        validate_sqlite_embedding_revision_contract(conn)?;
    }
    Ok(())
}

#[expect(clippy::string_slice, reason = "SQL lexer and closing-parenthesis indices are ASCII byte boundaries")]
fn validate_sqlite_fts_external_content(sql: &str) -> Result<(), StoreError> {
    let executable_sql = sqlite_sql_without_comments(sql);
    let arguments_start = sqlite_fts5_arguments_start(&executable_sql).ok_or_else(|| sqlite_source_schema_error("memory_fts is not an FTS5 virtual table"))?;
    let arguments_end = executable_sql
        .rfind(')')
        .filter(|end| *end >= arguments_start)
        .ok_or_else(|| sqlite_source_schema_error("memory_fts has malformed FTS5 arguments"))?;
    let mut options = HashMap::new();
    for argument in split_sqlite_fts_arguments(&executable_sql[arguments_start..arguments_end]) {
        let Some((key, value)) = argument.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = decode_sqlite_fts_option(value.trim()).to_ascii_lowercase();
        let _previous = options.insert(key, value);
    }
    if options.get("content").is_some_and(|value| value == "memories") && options.get("content_rowid").is_some_and(|value| value == "rowid") {
        Ok(())
    } else {
        Err(sqlite_source_schema_error("memory_fts must use memories(rowid) as its external-content source"))
    }
}

fn sqlite_sql_without_comments(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut executable = Vec::with_capacity(bytes.len());
    let mut index = 0_usize;
    while index < bytes.len() {
        let end = match bytes[index] {
            b'\'' | b'"' | b'`' => skip_sqlite_quoted(bytes, index, bytes[index]),
            b'[' => skip_sqlite_quoted(bytes, index, b']'),
            b'-' if bytes.get(index.saturating_add(1)) == Some(&b'-') => skip_sqlite_line_comment(bytes, index),
            b'/' if bytes.get(index.saturating_add(1)) == Some(&b'*') => skip_sqlite_block_comment(bytes, index),
            _ => {
                executable.push(bytes[index]);
                index = index.saturating_add(1);
                continue;
            }
        };
        if matches!(bytes[index], b'\'' | b'"' | b'`' | b'[') {
            if let Some(quoted) = bytes.get(index..end) {
                executable.extend_from_slice(quoted);
            }
        } else {
            executable.push(b' ');
        }
        index = end;
    }
    String::from_utf8(executable).unwrap_or_default()
}

fn sqlite_fts5_arguments_start(sql: &str) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut index = 0_usize;
    let mut saw_using = false;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`' | b'[') {
            let terminator = if bytes[index] == b'[' { b']' } else { bytes[index] };
            let end = skip_sqlite_quoted(bytes, index, terminator);
            if saw_using && sql.get(index..end).is_some_and(|quoted| decode_sqlite_fts_option(quoted).eq_ignore_ascii_case("fts5")) {
                return sqlite_open_parenthesis_after(bytes, end);
            }
            saw_using = false;
            index = end;
            continue;
        }
        match bytes[index] {
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index = index.saturating_add(1);
                while bytes.get(index).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
                    index = index.saturating_add(1);
                }
                let word = sql.get(start..index)?;
                if saw_using && word.eq_ignore_ascii_case("fts5") {
                    return sqlite_open_parenthesis_after(bytes, index);
                }
                saw_using = word.eq_ignore_ascii_case("using");
            }
            byte if byte.is_ascii_whitespace() => index = index.saturating_add(1),
            _ => {
                saw_using = false;
                index = index.saturating_add(1);
            }
        }
    }
    None
}

fn sqlite_open_parenthesis_after(bytes: &[u8], mut index: usize) -> Option<usize> {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index = index.saturating_add(1);
    }
    (bytes.get(index) == Some(&b'(')).then(|| index.saturating_add(1))
}

fn skip_sqlite_quoted(bytes: &[u8], start: usize, terminator: u8) -> usize {
    let mut index = start.saturating_add(1);
    while index < bytes.len() {
        if bytes[index] == terminator {
            if terminator != b']' && bytes.get(index.saturating_add(1)) == Some(&terminator) {
                index = index.saturating_add(2);
                continue;
            }
            return index.saturating_add(1);
        }
        index = index.saturating_add(1);
    }
    bytes.len()
}

fn skip_sqlite_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start.saturating_add(2);
    while bytes.get(index).is_some_and(|byte| !matches!(byte, b'\n' | b'\r')) {
        index = index.saturating_add(1);
    }
    index
}

fn skip_sqlite_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start.saturating_add(2);
    while index < bytes.len() {
        if bytes.get(index) == Some(&b'*') && bytes.get(index.saturating_add(1)) == Some(&b'/') {
            return index.saturating_add(2);
        }
        index = index.saturating_add(1);
    }
    bytes.len()
}

#[expect(clippy::string_slice, reason = "FTS delimiters and SQL quote bytes are ASCII byte boundaries")]
fn split_sqlite_fts_arguments(arguments: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0_usize;
    let mut quote = None;
    let bytes = arguments.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(active_quote) if byte == active_quote => {
                let doubled = active_quote != b']' && bytes.get(index.saturating_add(1)) == Some(&active_quote);
                quote = doubled.then_some(active_quote);
                index = index.saturating_add(usize::from(doubled));
            }
            None if matches!(byte, b'\'' | b'"' | b'`') => quote = Some(byte),
            None if byte == b'[' => quote = Some(b']'),
            None if byte == b',' => {
                result.push(&arguments[start..index]);
                start = index.saturating_add(1);
            }
            Some(_) | None => {}
        }
        index = index.saturating_add(1);
    }
    result.push(&arguments[start..]);
    result
}

#[expect(clippy::string_slice, reason = "recognized SQL quote delimiters are single-byte ASCII")]
fn decode_sqlite_fts_option(value: &str) -> String {
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let terminator = if first == b'[' { b']' } else { first };
        if matches!(first, b'\'' | b'"' | b'`' | b'[') && value.as_bytes().last() == Some(&terminator) {
            let inner = &value[1..value.len().saturating_sub(1)];
            if first != b'[' {
                let doubled = String::from_utf8(vec![terminator, terminator]).unwrap_or_default();
                let single = char::from(terminator).to_string();
                return inner.replace(&doubled, &single);
            }
            return inner.to_owned();
        }
    }
    value.to_owned()
}

fn validate_sqlite_embedding_revision_contract(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, bool>(3)?, row.get::<_, Option<String>>(4)?)))?;
    let mut contract = None;
    for row in rows {
        let (name, not_null, default) = row?;
        if name == "embedding_revision" {
            contract = Some((not_null, default));
            break;
        }
    }
    let Some((not_null, default)) = contract else {
        return Ok(());
    };
    let default_is_zero = default.as_deref().is_some_and(|value| value.trim().trim_matches(['(', ')']) == "0");
    if !not_null || !default_is_zero {
        return Err(sqlite_source_schema_error(
            "memories.embedding_revision must be NOT NULL with DEFAULT 0 because startup does not repair an existing definition",
        ));
    }
    Ok(())
}

fn validate_sqlite_primary_key(conn: &Connection, expectation: &SqliteKeyExpectation) -> Result<(), StoreError> {
    let actual = sqlite_primary_key_columns(conn, expectation.table)?;
    let expected = expectation.columns.iter().map(|column| (*column).to_owned()).collect::<Vec<_>>();
    if actual != expected {
        return Err(sqlite_source_schema_error(format!(
            "table {} has primary key ({}) but expected ({})",
            expectation.table,
            actual.join(", "),
            expected.join(", ")
        )));
    }
    Ok(())
}

fn validate_sqlite_context_unique_keys(conn: &Connection) -> Result<(), StoreError> {
    for expectation in SQLITE_CONTEXT_UNIQUE_KEYS {
        let mut index_statement = conn.prepare(&format!("PRAGMA index_list({})", quoted_sqlite_identifier(expectation.table)))?;
        let indexes = index_statement
            .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, bool>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut actual = BTreeSet::new();
        for (index, unique) in indexes {
            if !unique {
                continue;
            }
            let mut column_statement = conn.prepare(&format!("PRAGMA index_info({})", quoted_sqlite_identifier(&index)))?;
            let mut ordered_columns = column_statement
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            ordered_columns.sort_by_key(|(position, _)| *position);
            let _inserted = actual.insert(ordered_columns.into_iter().map(|(_, column)| column).collect::<Vec<_>>());
        }
        let expected = expectation
            .keys
            .iter()
            .map(|columns| columns.iter().map(|column| (*column).to_owned()).collect::<Vec<_>>())
            .collect::<BTreeSet<_>>();
        if actual != expected {
            return Err(sqlite_source_schema_error(format!(
                "table {} has unexpected UNIQUE keys: actual {actual:?}, expected {expected:?}",
                expectation.table
            )));
        }
    }
    Ok(())
}

fn validate_sqlite_context_foreign_keys(conn: &Connection) -> Result<(), StoreError> {
    let child_tables = SQLITE_CONTEXT_FOREIGN_KEYS.iter().map(|expectation| expectation.child_table).collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for child_table in child_tables {
        let mut statement = conn.prepare(&format!("PRAGMA foreign_key_list({})", quoted_sqlite_identifier(child_table)))?;
        let rows = statement.query_map([], |row| {
            Ok((
                child_table.to_owned(),
                row.get::<_, String>(3)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?.to_ascii_uppercase(),
                row.get::<_, String>(6)?.to_ascii_uppercase(),
                row.get::<_, String>(7)?.to_ascii_uppercase(),
            ))
        })?;
        actual.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let expected = SQLITE_CONTEXT_FOREIGN_KEYS
        .iter()
        .map(|expectation| {
            (
                expectation.child_table.to_owned(),
                expectation.child_column.to_owned(),
                expectation.parent_table.to_owned(),
                expectation.parent_column.to_owned(),
                expectation.update_action.to_owned(),
                expectation.delete_action.to_owned(),
                expectation.match_clause.to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(sqlite_source_schema_error(format!(
            "governed context foreign-key declarations differ from the current manifest: actual {actual:?}, expected {expected:?}"
        )));
    }
    Ok(())
}

fn validate_sqlite_context_integrity(conn: &Connection) -> Result<(), StoreError> {
    let has_cycle: bool = conn.query_row(
        "WITH RECURSIVE rooted(id) AS (
             SELECT id
             FROM contexts
             WHERE parent_id IS NULL
             UNION ALL
             SELECT child.id
             FROM rooted AS parent
             JOIN contexts AS child ON child.parent_id = parent.id
         )
         SELECT EXISTS(
             SELECT 1
             FROM contexts
             WHERE id NOT IN (SELECT id FROM rooted)
         )",
        [],
        |row| row.get(0),
    )?;
    if has_cycle {
        return Err(sqlite_source_schema_error("context hierarchy contains a parent cycle"));
    }
    let invalid_ordinals: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM memory_contexts
             GROUP BY memory_id
             HAVING MIN(ordinal) <> 0 OR MAX(ordinal) <> COUNT(*) - 1
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid_ordinals {
        return Err(sqlite_source_schema_error("memory context ordinals must be contiguous and start at zero"));
    }
    let mismatched_primary: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM memory_contexts AS membership
             JOIN contexts AS context_row ON context_row.id = membership.context_id
             JOIN memory_metadata AS metadata ON metadata.memory_id = membership.memory_id
             WHERE membership.ordinal = 0
               AND NULLIF(trim(metadata.scope_key), '') IS NOT NULL
               AND metadata.scope_key <> context_row.context_key
         )",
        [],
        |row| row.get(0),
    )?;
    if mismatched_primary {
        return Err(sqlite_source_schema_error("memory_metadata.scope_key must equal the ordinal-zero governed context key"));
    }
    Ok(())
}

fn validate_sqlite_schema_object_exists(conn: &Connection, object_type: &'static str, name: &'static str) -> Result<(), StoreError> {
    if sqlite_schema_sql(conn, object_type, name)?.is_none() {
        return Err(sqlite_source_schema_error(format!("required {object_type} {name} is missing")));
    }
    Ok(())
}

fn sqlite_schema_sql(conn: &Connection, object_type: &'static str, name: &'static str) -> Result<Option<String>, StoreError> {
    conn.query_row("SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2", [object_type, name], |row| row.get(0))
        .optional()
        .map_err(StoreError::from)
}

fn sqlite_table_columns(conn: &Connection, table: &'static str) -> Result<HashSet<String>, StoreError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quoted_sqlite_identifier(table)))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<Result<HashSet<_>, _>>().map_err(StoreError::from)
}

fn sqlite_primary_key_columns(conn: &Connection, table: &'static str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quoted_sqlite_identifier(table)))?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?)))?;
    let mut primary_key_columns = Vec::new();
    for row in rows {
        let (name, pk_order) = row?;
        if pk_order > 0 {
            primary_key_columns.push((pk_order, name));
        }
    }
    primary_key_columns.sort_by_key(|(pk_order, _)| *pk_order);
    Ok(primary_key_columns.into_iter().map(|(_, name)| name).collect())
}

fn quoted_sqlite_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn normalize_sqlite_ddl(value: &str) -> String {
    value.to_ascii_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_sqlite_check_constraints(expectation: &SqliteTableExpectation, sql: &str) -> Result<(), StoreError> {
    let actual = sqlite_check_constraints(sql);
    let expected = expectation
        .ddl_contains
        .iter()
        .filter(|fragment| fragment.trim_start().to_ascii_lowercase().starts_with("check"))
        .map(|fragment| normalize_sqlite_ddl(fragment))
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(sqlite_source_schema_error(format!(
            "table {} has unexpected CHECK constraints: actual {actual:?}, expected {expected:?}",
            expectation.name
        )));
    }
    Ok(())
}

#[expect(clippy::string_slice, reason = "SQL CHECK keywords, quotes, and parentheses are ASCII byte boundaries")]
fn sqlite_check_constraints(sql: &str) -> BTreeSet<String> {
    let executable = sqlite_sql_without_comments(sql);
    let bytes = executable.as_bytes();
    let mut checks = BTreeSet::new();
    let mut index = 0_usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`' | b'[') {
            let terminator = if bytes[index] == b'[' { b']' } else { bytes[index] };
            index = skip_sqlite_quoted(bytes, index, terminator);
            continue;
        }
        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let word_start = index;
            index = index.saturating_add(1);
            while bytes.get(index).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
                index = index.saturating_add(1);
            }
            if !executable[word_start..index].eq_ignore_ascii_case("check") {
                continue;
            }
            let mut open = index;
            while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                open = open.saturating_add(1);
            }
            if bytes.get(open) != Some(&b'(') {
                continue;
            }
            if let Some(end) = sqlite_check_constraint_end(bytes, open) {
                let _inserted = checks.insert(normalize_sqlite_ddl(&executable[word_start..end]));
                index = end;
            }
            continue;
        }
        index = index.saturating_add(1);
    }
    checks
}

fn sqlite_check_constraint_end(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    let mut depth = 0_usize;
    while cursor < bytes.len() {
        if matches!(bytes[cursor], b'\'' | b'"' | b'`' | b'[') {
            let terminator = if bytes[cursor] == b'[' { b']' } else { bytes[cursor] };
            cursor = skip_sqlite_quoted(bytes, cursor, terminator);
            continue;
        }
        match bytes[cursor] {
            b'(' => depth = depth.saturating_add(1),
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(cursor.saturating_add(1));
                }
            }
            _ => {}
        }
        cursor = cursor.saturating_add(1);
    }
    None
}

pub(crate) fn validate_sqlite_foreign_key_integrity(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    let mut violation_count = 0_u64;
    let mut first_violation = None;
    while let Some(row) = rows.next()? {
        violation_count = violation_count.saturating_add(1);
        if first_violation.is_none() {
            let table: String = row.get(0)?;
            let rowid: Option<i64> = row.get(1)?;
            let parent: String = row.get(2)?;
            let foreign_key_id: i64 = row.get(3)?;
            first_violation = Some(rowid.map_or_else(
                || format!("{table} references missing {parent} row through foreign key {foreign_key_id}"),
                |rowid| format!("{table}.rowid={rowid} references missing {parent} row through foreign key {foreign_key_id}"),
            ));
        }
    }
    if violation_count > 0 {
        let sample = first_violation.unwrap_or_else(|| "unknown violation".into());
        return Err(StoreError::Conflict(format!(
            "SQLite source contains data integrity violations: found {violation_count} foreign key violation(s), including {sample}; repair orphan rows or restore from backup before migration"
        )));
    }
    Ok(())
}

pub(crate) fn validate_embedding_map_integrity(conn: &Connection) -> Result<(), StoreError> {
    let orphan_count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM memory_embedding_map AS map
         LEFT JOIN memories AS memory ON memory.id = map.memory_id
         WHERE memory.id IS NULL",
        [],
        |row| row.get(0),
    )?;
    if orphan_count > 0 {
        return Err(StoreError::Conflict(format!(
            "SQLite source has {orphan_count} embedding map row(s) without matching memories"
        )));
    }
    let dangling_vector_count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM memory_embedding_map AS map
         LEFT JOIN memory_embeddings AS embedding ON embedding.rowid = map.vec_rowid
         WHERE embedding.rowid IS NULL",
        [],
        |row| row.get(0),
    )?;
    if dangling_vector_count > 0 {
        return Err(StoreError::Conflict(format!(
            "SQLite source has {dangling_vector_count} embedding map row(s) whose vec_rowid does not exist in memory_embeddings"
        )));
    }
    let unmapped_vector_count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM memory_embeddings AS embedding
         LEFT JOIN memory_embedding_map AS map ON map.vec_rowid = embedding.rowid
         WHERE map.vec_rowid IS NULL",
        [],
        |row| row.get(0),
    )?;
    if unmapped_vector_count > 0 {
        return Err(StoreError::Conflict(format!(
            "SQLite source has {unmapped_vector_count} vector row(s) without a matching embedding map entry"
        )));
    }
    validate_embedding_flag_integrity(conn)
}

fn validate_embedding_flag_integrity(conn: &Connection) -> Result<(), StoreError> {
    let invalid_flag_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE has_embedding IS NULL OR typeof(has_embedding) <> 'integer' OR has_embedding NOT IN (0, 1)",
        [],
        |row| row.get(0),
    )?;
    if invalid_flag_count > 0 {
        return Err(StoreError::Conflict(format!(
            "SQLite source has {invalid_flag_count} memory row(s) with a non-canonical has_embedding value"
        )));
    }
    let missing_map_count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM memories AS memory
         LEFT JOIN memory_embedding_map AS map ON map.memory_id = memory.id
         WHERE memory.has_embedding = 1 AND map.memory_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    if missing_map_count > 0 {
        return Err(StoreError::Conflict(format!(
            "SQLite source has {missing_map_count} memory row(s) with has_embedding=true but no embedding map entry"
        )));
    }
    let unexpected_map_count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM memories AS memory
         JOIN memory_embedding_map AS map ON map.memory_id = memory.id
         WHERE memory.has_embedding = 0",
        [],
        |row| row.get(0),
    )?;
    if unexpected_map_count > 0 {
        return Err(StoreError::Conflict(format!(
            "SQLite source has {unexpected_map_count} memory row(s) with has_embedding=false but an embedding map entry exists"
        )));
    }
    Ok(())
}

fn sqlite_source_schema_error(message: impl std::fmt::Display) -> StoreError {
    StoreError::Conflict(format!(
        "SQLite source schema is not current: {message}; open the source database once with this localhold build to apply supported schema repairs before retrying"
    ))
}

fn sqlite_newer_source_schema_error(schema_version: u32) -> StoreError {
    StoreError::Conflict(format!(
        "SQLite source schema version {schema_version} is newer than this binary supports ({}); use a matching or newer localhold build",
        super::schema::SQLITE_SCHEMA_VERSION
    ))
}

fn fetch_sqlite_embeddings_strict(conn: &Connection, ids: &[MemoryId], embedding_dimensions: usize) -> Result<super::EmbeddingMap, StoreError> {
    if ids.is_empty() {
        return Ok(super::EmbeddingMap::new());
    }
    let id_strs: Vec<String> = ids.iter().map(ToString::to_string).collect();
    #[expect(clippy::arithmetic_side_effects, reason = "enumerate index + 1 cannot overflow for realistic ID counts")]
    let placeholders = id_strs.iter().enumerate().map(|(idx, _)| format!("?{}", idx + 1)).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT map.memory_id, embedding.embedding
         FROM memory_embedding_map AS map
         JOIN memory_embeddings AS embedding ON embedding.rowid = map.vec_rowid
         WHERE map.memory_id IN ({placeholders})"
    );
    let params: Vec<&dyn ToSql> = id_strs.iter().map(coerce_to_sql).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        let id_str: String = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok((id_str, blob))
    })?;

    let mut result = super::EmbeddingMap::new();
    for row in rows {
        let (id_str, blob) = row?;
        let id = parse_memory_id_store(&id_str, "memory_embedding_map.memory_id")?;
        let embedding = decode_sqlite_embedding(&blob, embedding_dimensions).ok_or_else(|| {
            StoreError::Conflict(format!(
                "memory {id} has invalid embedding blob length: expected {} bytes, got {}",
                embedding_dimensions.saturating_mul(size_of::<f32>()),
                blob.len()
            ))
        })?;
        validate_embedding_vector(&embedding, embedding_dimensions)?;
        let _previous = result.insert(id, embedding);
    }
    Ok(result)
}

fn decode_sqlite_embedding(blob: &[u8], embedding_dimensions: usize) -> Option<Vec<f32>> {
    if blob.len() != embedding_dimensions.saturating_mul(size_of::<f32>()) {
        return None;
    }
    let chunks = blob.chunks_exact(size_of::<f32>());
    if !chunks.remainder().is_empty() {
        return None;
    }
    chunks.map(|chunk| chunk.try_into().map(f32::from_ne_bytes).ok()).collect()
}

fn coerce_to_sql(value: &String) -> &dyn ToSql {
    value
}

fn export_audit_entries(conn: &Connection) -> Result<Vec<MigrationAuditEntry>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, memory_id, action, caller_agent, timestamp, details
         FROM memory_audit_log
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let memory_id_str: String = row.get(1)?;
        let action_str: String = row.get(2)?;
        let timestamp_str: String = row.get(4)?;
        let details_json: Option<String> = row.get(5)?;
        let memory_id = parse_memory_id_sql(&memory_id_str, 1)?;
        let action = parse_enum_sql::<AuditAction>(&action_str, 2)?;
        let timestamp = parse_datetime_sql(&timestamp_str, 4)?;
        let details = details_json
            .map(|json| serde_json::from_str(&json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))))
            .transpose()?;
        Ok(MigrationAuditEntry {
            id: row.get(0)?,
            memory_id,
            entry: AuditEntry {
                action,
                caller_agent: row.get(3)?,
                timestamp,
                details,
            },
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

fn export_tombstones(conn: &Connection) -> Result<Vec<MigrationTombstone>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT memory_id, provenance, access_policy, deleted_at, deleted_by_principal
         FROM memory_tombstones
         ORDER BY memory_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let memory_id_str: String = row.get(0)?;
        let provenance_json: String = row.get(1)?;
        let access_policy_json: String = row.get(2)?;
        let deleted_at_str: String = row.get(3)?;
        Ok(MigrationTombstone {
            tombstone: MemoryTombstone {
                memory_id: parse_memory_id_sql(&memory_id_str, 0)?,
                provenance: parse_json_sql::<Provenance>(&provenance_json, 1)?,
                access_policy: parse_json_sql::<AccessPolicy>(&access_policy_json, 2)?,
                deleted_at: parse_datetime_sql(&deleted_at_str, 3)?,
                deleted_by_principal: row.get(4)?,
            },
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

#[expect(clippy::too_many_lines, reason = "all normalized context tables are exported together to preserve referential order")]
fn export_sqlite_contexts(conn: &Connection) -> Result<MigrationContextData, StoreError> {
    let kinds = {
        let mut statement = conn.prepare("SELECT kind, display_name, builtin, enabled, created_at, updated_at FROM context_kinds ORDER BY kind")?;
        statement
            .query_map([], |row| {
                let created_at: String = row.get(4)?;
                let updated_at: String = row.get(5)?;
                Ok(MigrationContextKind {
                    kind: row.get(0)?,
                    display_name: row.get(1)?,
                    builtin: row.get(2)?,
                    enabled: row.get(3)?,
                    created_at: parse_datetime_sql(&created_at, 4)?,
                    updated_at: parse_datetime_sql(&updated_at, 5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let contexts = {
        let mut statement = conn.prepare(
            "SELECT id, kind, context_key, normalized_key, display_name, description,
                    owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
             FROM contexts ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let kind: String = row.get(1)?;
                let parent_id: Option<String> = row.get(8)?;
                let lifecycle: String = row.get(9)?;
                let created_at: String = row.get(11)?;
                let updated_at: String = row.get(12)?;
                Ok(MigrationContext {
                    definition: ContextDefinition {
                        id: parse_context_id_sql(&id, 0)?,
                        kind: ContextKind::new(kind).map_err(|error| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error)))?,
                        key: row.get(2)?,
                        display_name: row.get(4)?,
                        description: row.get(5)?,
                        owner_principal: row.get(6)?,
                        guidance: row.get(7)?,
                        parent_id: parent_id.as_deref().map(|value| parse_context_id_sql(value, 8)).transpose()?,
                        lifecycle: lifecycle
                            .parse()
                            .map_err(|error: ParseEnumError| rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(error)))?,
                        frozen: row.get(10)?,
                        created_at: parse_datetime_sql(&created_at, 11)?,
                        updated_at: parse_datetime_sql(&updated_at, 12)?,
                    },
                    normalized_key: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let aliases = {
        let mut statement = conn.prepare("SELECT context_id, alias, normalized_alias, created_at FROM context_aliases ORDER BY context_id, normalized_alias")?;
        statement
            .query_map([], |row| {
                let context_id: String = row.get(0)?;
                let created_at: String = row.get(3)?;
                Ok(MigrationContextAlias {
                    context_id: parse_context_id_sql(&context_id, 0)?,
                    alias: row.get(1)?,
                    normalized_alias: row.get(2)?,
                    created_at: parse_datetime_sql(&created_at, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let identities = {
        let mut statement = conn.prepare(
            "SELECT context_id, owner_principal, kind, scheme, namespace,
                    fingerprint, redacted_label, created_at
             FROM context_identities
             ORDER BY context_id, scheme, namespace, fingerprint",
        )?;
        statement
            .query_map([], |row| {
                let context_id: String = row.get(0)?;
                let namespace: String = row.get(4)?;
                let created_at: String = row.get(7)?;
                Ok(MigrationContextIdentity {
                    context_id: parse_context_id_sql(&context_id, 0)?,
                    owner_principal: row.get(1)?,
                    kind: row.get(2)?,
                    identity: ContextIdentity {
                        scheme: row.get(3)?,
                        namespace: (!namespace.is_empty()).then_some(namespace),
                        fingerprint: row.get(5)?,
                        redacted_label: row.get(6)?,
                    },
                    created_at: parse_datetime_sql(&created_at, 7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let hints = {
        let mut statement = conn.prepare("SELECT context_id, hint, normalized_hint, created_at FROM context_resolver_hints ORDER BY context_id, normalized_hint")?;
        statement
            .query_map([], |row| {
                let context_id: String = row.get(0)?;
                let created_at: String = row.get(3)?;
                Ok(MigrationContextHint {
                    context_id: parse_context_id_sql(&context_id, 0)?,
                    hint: row.get(1)?,
                    normalized_hint: row.get(2)?,
                    created_at: parse_datetime_sql(&created_at, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let grants = {
        let mut statement = conn.prepare("SELECT context_id, grantee_principal, granted_by, created_at FROM context_grants ORDER BY context_id, grantee_principal")?;
        statement
            .query_map([], |row| {
                let context_id: String = row.get(0)?;
                let created_at: String = row.get(3)?;
                Ok(ContextGrant {
                    context_id: parse_context_id_sql(&context_id, 0)?,
                    grantee_principal: row.get(1)?,
                    granted_by: row.get(2)?,
                    created_at: parse_datetime_sql(&created_at, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let relations = {
        let mut statement = conn.prepare("SELECT from_context_id, to_context_id, relation, created_at FROM context_relations ORDER BY from_context_id, to_context_id, relation")?;
        statement
            .query_map([], |row| {
                let from_context_id: String = row.get(0)?;
                let to_context_id: String = row.get(1)?;
                let created_at: String = row.get(3)?;
                Ok(MigrationContextRelation {
                    from_context_id: parse_context_id_sql(&from_context_id, 0)?,
                    to_context_id: parse_context_id_sql(&to_context_id, 1)?,
                    relation: row.get(2)?,
                    created_at: parse_datetime_sql(&created_at, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let memberships = {
        let mut statement = conn.prepare("SELECT memory_id, context_id, ordinal, created_at FROM memory_contexts ORDER BY memory_id, ordinal")?;
        statement
            .query_map([], |row| {
                let memory_id: String = row.get(0)?;
                let context_id: String = row.get(1)?;
                let created_at: String = row.get(3)?;
                Ok(MigrationMemoryContext {
                    memory_id: parse_memory_id_sql(&memory_id, 0)?,
                    context_id: parse_context_id_sql(&context_id, 1)?,
                    ordinal: row.get(2)?,
                    created_at: parse_datetime_sql(&created_at, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let policies = {
        let mut statement = conn.prepare("SELECT layer, principal, kind, policy_json, updated_at FROM context_kind_policies ORDER BY layer, principal, kind")?;
        statement
            .query_map([], |row| {
                let policy: String = row.get(3)?;
                let updated_at: String = row.get(4)?;
                Ok(MigrationContextPolicy {
                    layer: row.get(0)?,
                    principal: row.get(1)?,
                    kind: row.get(2)?,
                    policy: parse_json_sql(&policy, 3)?,
                    updated_at: parse_datetime_sql(&updated_at, 4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let anchor_overrides = {
        let mut statement = conn.prepare("SELECT anchor_context_id, principal, policy_json, updated_at FROM context_anchor_overrides ORDER BY anchor_context_id, principal")?;
        statement
            .query_map([], |row| {
                let anchor_context_id: String = row.get(0)?;
                let policy: String = row.get(2)?;
                let updated_at: String = row.get(3)?;
                Ok(MigrationContextAnchorOverride {
                    anchor_context_id: parse_context_id_sql(&anchor_context_id, 0)?,
                    principal: row.get(1)?,
                    policy: parse_json_sql(&policy, 2)?,
                    updated_at: parse_datetime_sql(&updated_at, 3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let audit_events = {
        let mut statement = conn.prepare(
            "SELECT id, actor_principal, action, context_id, memory_id, timestamp, details
             FROM context_audit_events ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                let context_id: Option<String> = row.get(3)?;
                let memory_id: Option<String> = row.get(4)?;
                let timestamp: String = row.get(5)?;
                let details: Option<String> = row.get(6)?;
                Ok(ContextAuditEvent {
                    id: row.get(0)?,
                    actor_principal: row.get(1)?,
                    action: row.get(2)?,
                    context_id: context_id.as_deref().map(|value| parse_context_id_sql(value, 3)).transpose()?,
                    memory_id: memory_id.as_deref().map(|value| parse_memory_id_sql(value, 4)).transpose()?,
                    timestamp: parse_datetime_sql(&timestamp, 5)?,
                    details: details.as_deref().map(|value| parse_json_sql(value, 6)).transpose()?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(MigrationContextData {
        kinds,
        contexts,
        aliases,
        identities,
        hints,
        grants,
        relations,
        memberships,
        policies,
        anchor_overrides,
        audit_events,
    })
}

fn export_metadata(conn: &Connection) -> Result<Vec<MigrationMetadata>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT memory_id, scope_key, summary, agent_label, created_by_principal, quality_flags, schema_version, migrated_at, updated_at
         FROM memory_metadata
         ORDER BY memory_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let memory_id_str: String = row.get(0)?;
        let quality_flags_json: String = row.get(5)?;
        let migrated_at_str: Option<String> = row.get(7)?;
        let updated_at_str: String = row.get(8)?;
        Ok(MigrationMetadata {
            metadata: MemoryMetadata {
                memory_id: parse_memory_id_sql(&memory_id_str, 0)?,
                scope_key: row.get(1)?,
                summary: row.get(2)?,
                agent_label: row.get(3)?,
                created_by_principal: row.get(4)?,
                quality_flags: parse_json_sql(&quality_flags_json, 5)?,
                schema_version: row.get(6)?,
            },
            migrated_at: migrated_at_str.as_deref().map(|value| parse_datetime_sql(value, 7)).transpose()?,
            updated_at: parse_datetime_sql(&updated_at_str, 8)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}

async fn export_postgres_tx(tx: &mut Transaction<'_, Postgres>, embedding_dimensions: usize) -> Result<MigrationSnapshot, StoreError> {
    let memory_rows = query(AssertSqlSafe(format!(
        "SELECT {MEMORY_COLUMNS}, embedding_revision FROM memories ORDER BY created_at ASC, id ASC"
    )))
    .fetch_all(&mut **tx)
    .await?;
    let mut memories = memory_rows
        .iter()
        .map(|row| Ok((postgres_row_to_memory(row)?, row.try_get::<i64, _>("embedding_revision")?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    let ids: Vec<MemoryId> = memories.iter().map(|(memory, _)| memory.id).collect();
    let entity_map = fetch_postgres_entities_tx(tx, &ids).await?;
    for (memory, _) in &mut memories {
        memory.entities = entity_map.get(&memory.id).cloned().unwrap_or_default();
    }

    let mut embeddings = fetch_postgres_embeddings_tx(tx, &ids, embedding_dimensions).await?;
    let mut migration_memories = Vec::with_capacity(memories.len());
    let mut superseded_links = Vec::new();
    for (mut memory, embedding_revision) in memories {
        let embedding = embeddings.remove(&memory.id);
        if memory.has_embedding && embedding.is_none() {
            return Err(StoreError::Conflict(format!(
                "target memory {} has has_embedding=true but no valid embedding vector",
                memory.id
            )));
        }
        if !memory.has_embedding && embedding.is_some() {
            return Err(StoreError::Conflict(format!(
                "target memory {} has has_embedding=false but still has an embedding vector",
                memory.id
            )));
        }
        if let Some(superseded_by) = memory.superseded_by.take() {
            superseded_links.push((memory.id, superseded_by));
        }
        migration_memories.push(MigrationMemory {
            memory,
            embedding_revision,
            embedding,
        });
    }

    Ok(MigrationSnapshot {
        memories: migration_memories,
        superseded_links,
        audit_entries: export_postgres_audit_entries_tx(tx).await?,
        tombstones: export_postgres_tombstones_tx(tx).await?,
        contexts: export_postgres_contexts_tx(tx).await?,
        metadata: export_postgres_metadata_tx(tx).await?,
        embedding_profile: export_postgres_embedding_profile_tx(tx).await?,
        counts: postgres_counts_tx(tx).await?,
    })
}

async fn fetch_postgres_entities_tx(tx: &mut Transaction<'_, Postgres>, ids: &[MemoryId]) -> Result<HashMap<MemoryId, Vec<Entity>>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let id_strs = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    let rows = query(
        "SELECT memory_id, entity, entity_type
         FROM memory_entities
         WHERE memory_id = ANY($1)
         ORDER BY memory_id ASC, entity ASC, entity_type ASC",
    )
    .bind(id_strs)
    .fetch_all(&mut **tx)
    .await?;

    let mut result: HashMap<MemoryId, Vec<Entity>> = HashMap::new();
    for row in rows {
        let id_str: String = row.try_get("memory_id")?;
        let entity_type: String = row.try_get("entity_type")?;
        let id = parse_memory_id_store(&id_str, "memory_entities.memory_id")?;
        result.entry(id).or_default().push(Entity {
            name: row.try_get("entity")?,
            entity_type: entity_type.try_into().map_err(|e: ParseEnumError| StoreError::Serialization(Box::new(e)))?,
        });
    }
    Ok(result)
}

async fn fetch_postgres_embeddings_tx(tx: &mut Transaction<'_, Postgres>, ids: &[MemoryId], embedding_dimensions: usize) -> Result<super::EmbeddingMap, StoreError> {
    if ids.is_empty() {
        return Ok(super::EmbeddingMap::new());
    }
    let id_strs = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    let rows = query(
        "SELECT memory_id, embedding::text AS embedding
         FROM memory_embeddings
         WHERE memory_id = ANY($1)",
    )
    .bind(id_strs)
    .fetch_all(&mut **tx)
    .await?;

    let mut result = super::EmbeddingMap::new();
    for row in rows {
        let id_str: String = row.try_get("memory_id")?;
        let embedding_text: String = row.try_get("embedding")?;
        let id = parse_memory_id_store(&id_str, "memory_embeddings.memory_id")?;
        let embedding = parse_pgvector_text(&embedding_text).ok_or_else(|| invalid_pgvector_text_error(&id, &embedding_text))?;
        if embedding.len() != embedding_dimensions {
            return Err(StoreError::Conflict(format!(
                "target memory {id} has embedding dimension {}, expected {embedding_dimensions}",
                embedding.len()
            )));
        }
        let _previous = result.insert(id, embedding);
    }
    Ok(result)
}

fn invalid_pgvector_text_error(id: &MemoryId, text: &str) -> StoreError {
    StoreError::Conflict(format!("target memory {id} has invalid pgvector text ({} bytes)", text.len()))
}

fn parse_pgvector_text(raw: &str) -> Option<Vec<f32>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner.split(',').map(|value| value.trim().parse::<f32>().ok()).collect()
}

async fn export_postgres_audit_entries_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<MigrationAuditEntry>, StoreError> {
    let rows = query(
        "SELECT id, memory_id, action, caller_agent, timestamp, details
         FROM memory_audit_log
         ORDER BY id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(postgres_row_to_audit_entry).collect()
}

async fn export_postgres_tombstones_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<MigrationTombstone>, StoreError> {
    let rows = query(
        "SELECT memory_id, provenance, access_policy, deleted_at, deleted_by_principal
         FROM memory_tombstones
         ORDER BY memory_id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(postgres_row_to_tombstone).collect()
}

#[expect(clippy::too_many_lines, reason = "all normalized context tables are exported together to preserve referential order")]
async fn export_postgres_contexts_tx(tx: &mut Transaction<'_, Postgres>) -> Result<MigrationContextData, StoreError> {
    let kinds = query("SELECT kind, display_name, builtin, enabled, created_at, updated_at FROM context_kinds ORDER BY kind")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            Ok(MigrationContextKind {
                kind: row.try_get("kind")?,
                display_name: row.try_get("display_name")?,
                builtin: row.try_get("builtin")?,
                enabled: row.try_get("enabled")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let contexts = query(
        "SELECT id, kind, context_key, normalized_key, display_name, description,
                owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
         FROM contexts ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|row| {
        let id: String = row.try_get("id")?;
        let kind: String = row.try_get("kind")?;
        let parent_id: Option<String> = row.try_get("parent_id")?;
        let lifecycle: String = row.try_get("lifecycle")?;
        Ok(MigrationContext {
            definition: ContextDefinition {
                id: parse_context_id_store(&id, "contexts.id")?,
                kind: ContextKind::new(kind).map_err(|error| StoreError::Serialization(Box::new(error)))?,
                key: row.try_get("context_key")?,
                display_name: row.try_get("display_name")?,
                description: row.try_get("description")?,
                owner_principal: row.try_get("owner_principal")?,
                guidance: row.try_get("guidance")?,
                parent_id: parent_id.as_deref().map(|value| parse_context_id_store(value, "contexts.parent_id")).transpose()?,
                lifecycle: lifecycle.parse().map_err(|error: ParseEnumError| StoreError::Serialization(Box::new(error)))?,
                frozen: row.try_get("frozen")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            },
            normalized_key: row.try_get("normalized_key")?,
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    let aliases = query("SELECT context_id, alias, normalized_alias, created_at FROM context_aliases ORDER BY context_id, normalized_alias")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            let context_id: String = row.try_get("context_id")?;
            Ok(MigrationContextAlias {
                context_id: parse_context_id_store(&context_id, "context_aliases.context_id")?,
                alias: row.try_get("alias")?,
                normalized_alias: row.try_get("normalized_alias")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let identities = query(
        "SELECT context_id, owner_principal, kind, scheme, namespace,
                fingerprint, redacted_label, created_at
         FROM context_identities
         ORDER BY context_id, scheme, namespace, fingerprint",
    )
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|row| {
        let context_id: String = row.try_get("context_id")?;
        let namespace: String = row.try_get("namespace")?;
        Ok(MigrationContextIdentity {
            context_id: parse_context_id_store(&context_id, "context_identities.context_id")?,
            owner_principal: row.try_get("owner_principal")?,
            kind: row.try_get("kind")?,
            identity: ContextIdentity {
                scheme: row.try_get("scheme")?,
                namespace: (!namespace.is_empty()).then_some(namespace),
                fingerprint: row.try_get("fingerprint")?,
                redacted_label: row.try_get("redacted_label")?,
            },
            created_at: row.try_get("created_at")?,
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    let hints = query("SELECT context_id, hint, normalized_hint, created_at FROM context_resolver_hints ORDER BY context_id, normalized_hint")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            let context_id: String = row.try_get("context_id")?;
            Ok(MigrationContextHint {
                context_id: parse_context_id_store(&context_id, "context_resolver_hints.context_id")?,
                hint: row.try_get("hint")?,
                normalized_hint: row.try_get("normalized_hint")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let grants = query("SELECT context_id, grantee_principal, granted_by, created_at FROM context_grants ORDER BY context_id, grantee_principal")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            let context_id: String = row.try_get("context_id")?;
            Ok(ContextGrant {
                context_id: parse_context_id_store(&context_id, "context_grants.context_id")?,
                grantee_principal: row.try_get("grantee_principal")?,
                granted_by: row.try_get("granted_by")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let relations = query(
        "SELECT from_context_id, to_context_id, relation, created_at
         FROM context_relations ORDER BY from_context_id, to_context_id, relation",
    )
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|row| {
        let from_context_id: String = row.try_get("from_context_id")?;
        let to_context_id: String = row.try_get("to_context_id")?;
        Ok(MigrationContextRelation {
            from_context_id: parse_context_id_store(&from_context_id, "context_relations.from_context_id")?,
            to_context_id: parse_context_id_store(&to_context_id, "context_relations.to_context_id")?,
            relation: row.try_get("relation")?,
            created_at: row.try_get("created_at")?,
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    let memberships = query("SELECT memory_id, context_id, ordinal, created_at FROM memory_contexts ORDER BY memory_id, ordinal")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            let memory_id: String = row.try_get("memory_id")?;
            let context_id: String = row.try_get("context_id")?;
            Ok(MigrationMemoryContext {
                memory_id: parse_memory_id_store(&memory_id, "memory_contexts.memory_id")?,
                context_id: parse_context_id_store(&context_id, "memory_contexts.context_id")?,
                ordinal: row.try_get("ordinal")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let policies = query("SELECT layer, principal, kind, policy_json, updated_at FROM context_kind_policies ORDER BY layer, principal, kind")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            let Json(policy): Json<serde_json::Value> = row.try_get("policy_json")?;
            Ok(MigrationContextPolicy {
                layer: row.try_get("layer")?,
                principal: row.try_get("principal")?,
                kind: row.try_get("kind")?,
                policy,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let anchor_overrides = query("SELECT anchor_context_id, principal, policy_json, updated_at FROM context_anchor_overrides ORDER BY anchor_context_id, principal")
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .map(|row| {
            let anchor_context_id: String = row.try_get("anchor_context_id")?;
            let Json(policy): Json<serde_json::Value> = row.try_get("policy_json")?;
            Ok(MigrationContextAnchorOverride {
                anchor_context_id: parse_context_id_store(&anchor_context_id, "context_anchor_overrides.anchor_context_id")?,
                principal: row.try_get("principal")?,
                policy,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let audit_events = query(
        "SELECT id, actor_principal, action, context_id, memory_id, timestamp, details
         FROM context_audit_events ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|row| {
        let context_id: Option<String> = row.try_get("context_id")?;
        let memory_id: Option<String> = row.try_get("memory_id")?;
        let details: Option<Json<serde_json::Value>> = row.try_get("details")?;
        Ok(ContextAuditEvent {
            id: row.try_get("id")?,
            actor_principal: row.try_get("actor_principal")?,
            action: row.try_get("action")?,
            context_id: context_id
                .as_deref()
                .map(|value| parse_context_id_store(value, "context_audit_events.context_id"))
                .transpose()?,
            memory_id: memory_id
                .as_deref()
                .map(|value| parse_memory_id_store(value, "context_audit_events.memory_id"))
                .transpose()?,
            timestamp: row.try_get("timestamp")?,
            details: details.map(|Json(value)| value),
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(MigrationContextData {
        kinds,
        contexts,
        aliases,
        identities,
        hints,
        grants,
        relations,
        memberships,
        policies,
        anchor_overrides,
        audit_events,
    })
}

async fn export_postgres_metadata_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<MigrationMetadata>, StoreError> {
    let rows = query(
        "SELECT memory_id, scope_key, summary, agent_label, created_by_principal, quality_flags, schema_version, migrated_at, updated_at
         FROM memory_metadata
         ORDER BY memory_id ASC",
    )
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(postgres_row_to_metadata).collect()
}

async fn export_postgres_embedding_profile_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Option<EmbeddingProfile>, StoreError> {
    let row = query("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1")
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|row| {
        let dimensions: i64 = row.try_get("dimensions")?;
        Ok(EmbeddingProfile {
            provider: row.try_get("provider")?,
            endpoint: row.try_get("endpoint")?,
            model: row.try_get("model")?,
            dimensions: usize::try_from(dimensions).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        })
    })
    .transpose()
}

fn postgres_row_to_memory(row: &PgRow) -> Result<Memory, StoreError> {
    let id_str: String = row.try_get("id")?;
    let tags: Json<Vec<String>> = row.try_get("tags")?;
    let provenance: Json<Provenance> = row.try_get("provenance")?;
    let access_policy: Json<AccessPolicy> = row.try_get("access_policy")?;
    let memory_type: String = row.try_get("memory_type")?;
    let superseded_by: Option<String> = row.try_get("superseded_by")?;
    let impression_count: i64 = row.try_get("impression_count")?;
    Ok(Memory {
        id: parse_memory_id_store(&id_str, "memories.id")?,
        content: row.try_get("content")?,
        tags: tags.0,
        provenance: provenance.0,
        access_policy: access_policy.0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        record_revision: row.try_get("record_revision")?,
        expires_at: row.try_get("expires_at")?,
        has_embedding: row.try_get("has_embedding")?,
        memory_type: memory_type.parse().map_err(|e: ParseEnumError| StoreError::Serialization(Box::new(e)))?,
        importance: crate::types::Importance::new(row.try_get("importance")?),
        confidence: crate::types::Confidence::new(row.try_get("confidence")?),
        impression_count: u64::try_from(impression_count).map_err(|e| StoreError::Serialization(Box::new(e)))?,
        last_impressed_at: row.try_get("last_impressed_at")?,
        superseded_by: superseded_by.as_deref().map(|value| parse_memory_id_store(value, "memories.superseded_by")).transpose()?,
        activity_mass: row.try_get("activity_mass")?,
        last_used_at: row.try_get("last_used_at")?,
        entities: Vec::new(),
        was_redacted: false,
    })
}

fn postgres_row_to_audit_entry(row: &PgRow) -> Result<MigrationAuditEntry, StoreError> {
    let memory_id: String = row.try_get("memory_id")?;
    let action: String = row.try_get("action")?;
    let details: Option<Json<serde_json::Value>> = row.try_get("details")?;
    Ok(MigrationAuditEntry {
        id: row.try_get("id")?,
        memory_id: parse_memory_id_store(&memory_id, "memory_audit_log.memory_id")?,
        entry: AuditEntry {
            action: action.parse().map_err(|e: ParseEnumError| StoreError::Serialization(Box::new(e)))?,
            caller_agent: row.try_get("caller_agent")?,
            timestamp: row.try_get("timestamp")?,
            details: details.map(|json| json.0),
        },
    })
}

fn postgres_row_to_tombstone(row: &PgRow) -> Result<MigrationTombstone, StoreError> {
    let memory_id: String = row.try_get("memory_id")?;
    let provenance: Json<Provenance> = row.try_get("provenance")?;
    let access_policy: Json<AccessPolicy> = row.try_get("access_policy")?;
    Ok(MigrationTombstone {
        tombstone: MemoryTombstone {
            memory_id: parse_memory_id_store(&memory_id, "memory_tombstones.memory_id")?,
            provenance: provenance.0,
            access_policy: access_policy.0,
            deleted_at: row.try_get("deleted_at")?,
            deleted_by_principal: row.try_get("deleted_by_principal")?,
        },
    })
}

fn postgres_row_to_metadata(row: &PgRow) -> Result<MigrationMetadata, StoreError> {
    let memory_id: String = row.try_get("memory_id")?;
    let quality_flags: Json<Vec<String>> = row.try_get("quality_flags")?;
    Ok(MigrationMetadata {
        metadata: MemoryMetadata {
            memory_id: parse_memory_id_store(&memory_id, "memory_metadata.memory_id")?,
            scope_key: row.try_get("scope_key")?,
            summary: row.try_get("summary")?,
            agent_label: row.try_get("agent_label")?,
            created_by_principal: row.try_get("created_by_principal")?,
            quality_flags: quality_flags.0,
            schema_version: row.try_get("schema_version")?,
        },
        migrated_at: row.try_get("migrated_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn parse_memory_id_sql(value: &str, column: usize) -> Result<MemoryId, rusqlite::Error> {
    value
        .parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_context_id_sql(value: &str, column: usize) -> Result<ContextId, rusqlite::Error> {
    value
        .parse()
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error)))
}

fn parse_memory_id_store(value: &str, field: &'static str) -> Result<MemoryId, StoreError> {
    value
        .parse()
        .map_err(|e| StoreError::Serialization(format!("invalid {field} memory id {value:?}: {e}").into()))
}

fn parse_context_id_store(value: &str, field: &'static str) -> Result<ContextId, StoreError> {
    value
        .parse()
        .map_err(|error| StoreError::Serialization(format!("invalid {field} context id {value:?}: {error}").into()))
}

fn parse_enum_sql<T>(value: &str, column: usize) -> Result<T, rusqlite::Error>
where
    T: std::str::FromStr<Err = ParseEnumError>,
{
    value
        .parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_datetime_sql(value: &str, column: usize) -> Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
}

fn parse_json_sql<T: serde::de::DeserializeOwned>(value: &str, column: usize) -> Result<T, rusqlite::Error> {
    serde_json::from_str(value).map_err(|e| rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
}

fn sqlite_counts(conn: &Connection) -> Result<MigrationTableCounts, StoreError> {
    Ok(MigrationTableCounts {
        memories: sqlite_count(conn, "memories")?,
        entities: sqlite_count(conn, "memory_entities")?,
        embeddings: sqlite_count(conn, "memory_embedding_map")?,
        audit_entries: sqlite_count(conn, "memory_audit_log")?,
        tombstones: sqlite_count(conn, "memory_tombstones")?,
        context_kinds: sqlite_count(conn, "context_kinds")?,
        contexts: sqlite_count(conn, "contexts")?,
        context_aliases: sqlite_count(conn, "context_aliases")?,
        context_identities: sqlite_count(conn, "context_identities")?,
        context_hints: sqlite_count(conn, "context_resolver_hints")?,
        context_grants: sqlite_count(conn, "context_grants")?,
        context_relations: sqlite_count(conn, "context_relations")?,
        memory_contexts: sqlite_count(conn, "memory_contexts")?,
        context_policies: sqlite_count(conn, "context_kind_policies")?,
        context_anchor_overrides: sqlite_count(conn, "context_anchor_overrides")?,
        context_audit_events: sqlite_count(conn, "context_audit_events")?,
        metadata: sqlite_count(conn, "memory_metadata")?,
        embedding_profiles: sqlite_count(conn, "embedding_profile")?,
    })
}

fn export_sqlite_embedding_profile(conn: &Connection) -> Result<Option<EmbeddingProfile>, StoreError> {
    conn.query_row("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1", [], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
    })
    .optional()?
    .map(|(provider, endpoint, model, dimensions)| {
        Ok(EmbeddingProfile {
            provider,
            endpoint,
            model,
            dimensions: usize::try_from(dimensions).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        })
    })
    .transpose()
}

fn sqlite_count(conn: &Connection, table: &'static str) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
    u64::try_from(count).map_err(|e| StoreError::Serialization(Box::new(e)))
}

fn validate_supersession_links(source: &MigrationSnapshot) -> Result<(), StoreError> {
    let ids: HashSet<MemoryId> = source.memories.iter().map(|memory| memory.memory.id).collect();
    for (id, superseded_by) in &source.superseded_links {
        if !ids.contains(superseded_by) {
            return Err(StoreError::Conflict(format!("memory {id} is superseded by missing memory {superseded_by}")));
        }
    }
    Ok(())
}

async fn open_postgres_pool(url: &str) -> Result<PgPool, StoreError> {
    PgPoolOptions::new().max_connections(1).connect(url).await.map_err(StoreError::from)
}

pub(crate) async fn validate_existing_postgres_schema(
    pool: &PgPool,
    embedding_dimensions: usize,
    current_schema_only: bool,
    include_migration_metadata: bool,
) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    validate_existing_postgres_schema_connection(&mut connection, embedding_dimensions, current_schema_only, include_migration_metadata).await
}

async fn validate_existing_postgres_schema_connection(
    connection: &mut PgConnection,
    embedding_dimensions: usize,
    current_schema_only: bool,
    include_migration_metadata: bool,
) -> Result<(), StoreError> {
    reject_retired_postgres_schema_connection(connection, current_schema_only).await?;
    if include_migration_metadata && postgres_table_exists_connection(connection, POSTGRES_MIGRATIONS_TABLE, current_schema_only).await? {
        validate_postgres_table_kind_connection(connection, POSTGRES_MIGRATIONS_TABLE, current_schema_only).await?;
        for expectation in POSTGRES_REQUIRED_COLUMNS.iter().filter(|expectation| expectation.table == POSTGRES_MIGRATIONS_TABLE) {
            validate_postgres_column_type_connection(connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
        }
        validate_postgres_table_contracts_connection(connection, POSTGRES_MIGRATIONS_TABLE, current_schema_only).await?;
        validate_postgres_required_keys_connection(connection, POSTGRES_MIGRATIONS_TABLE, current_schema_only).await?;
    }

    let mut existing = HashSet::new();
    for table in POSTGRES_USER_TABLES {
        if postgres_table_exists_connection(connection, table, current_schema_only).await? {
            validate_postgres_table_kind_connection(connection, table, current_schema_only).await?;
            let _inserted = existing.insert(*table);
        }
    }
    if existing.is_empty() {
        return Ok(());
    }

    let missing: Vec<_> = POSTGRES_USER_TABLES.iter().copied().filter(|table| !existing.contains(*table)).collect();
    if !missing.is_empty() {
        return Err(StoreError::Conflict(format!(
            "PostgreSQL target has a partial managed schema; missing tables: {}",
            missing.join(", ")
        )));
    }

    for expectation in POSTGRES_REQUIRED_COLUMNS.iter().filter(|expectation| expectation.table != POSTGRES_MIGRATIONS_TABLE) {
        validate_postgres_column_type_connection(connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
    }
    for expectation in POSTGRES_OPTIONAL_COLUMNS {
        validate_optional_postgres_column_type_connection(connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
    }
    for table in POSTGRES_USER_TABLES {
        validate_postgres_table_contracts_connection(connection, table, current_schema_only).await?;
        validate_postgres_required_keys_connection(connection, table, current_schema_only).await?;
    }
    validate_postgres_column_type_connection(
        connection,
        "memory_embeddings",
        "embedding",
        &format!("vector({embedding_dimensions})"),
        current_schema_only,
    )
    .await?;
    Ok(())
}

pub(crate) async fn validate_ready_postgres_schema(
    pool: &PgPool,
    embedding_dimensions: usize,
    current_schema_only: bool,
    include_migration_metadata: bool,
) -> Result<(), StoreError> {
    validate_existing_postgres_schema(pool, embedding_dimensions, current_schema_only, include_migration_metadata).await?;
    if postgres_table_exists(pool, "scope_registry", current_schema_only).await? {
        return Err(StoreError::Conflict("current PostgreSQL schema contains retired scope_registry".into()));
    }
    if !postgres_table_exists(pool, "memories", current_schema_only).await? {
        return Err(StoreError::Conflict(
            "PostgreSQL database is not initialized; enable database.postgres.auto_migrate or start LocalHold once with migrations enabled".into(),
        ));
    }
    let mut connection = pool.acquire().await?;
    for expectation in POSTGRES_OPTIONAL_COLUMNS {
        validate_postgres_column_type_connection(&mut connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
    }
    drop(connection);
    validate_postgres_runtime_relationships(pool, current_schema_only).await?;
    validate_postgres_context_integrity(pool).await?;
    if !postgres_runtime_indexes_compatible(pool, current_schema_only, false).await? {
        return Err(StoreError::Conflict("PostgreSQL managed schema indexes do not match runtime requirements".into()));
    }
    Ok(())
}

async fn validate_postgres_context_integrity(pool: &PgPool) -> Result<(), StoreError> {
    let has_cycle: bool = query_scalar(
        "WITH RECURSIVE rooted(id) AS (
             SELECT id
             FROM contexts
             WHERE parent_id IS NULL
             UNION ALL
             SELECT child.id
             FROM rooted AS parent
             JOIN contexts AS child ON child.parent_id = parent.id
         )
         SELECT EXISTS(
             SELECT 1
             FROM contexts
             WHERE id NOT IN (SELECT id FROM rooted)
         )",
    )
    .fetch_one(pool)
    .await?;
    if has_cycle {
        return Err(StoreError::Conflict("PostgreSQL context hierarchy contains a parent cycle".into()));
    }
    let invalid_ordinals: bool = query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM memory_contexts
             GROUP BY memory_id
             HAVING MIN(ordinal) <> 0 OR MAX(ordinal) <> COUNT(*) - 1
         )",
    )
    .fetch_one(pool)
    .await?;
    if invalid_ordinals {
        return Err(StoreError::Conflict("PostgreSQL memory context ordinals must be contiguous and start at zero".into()));
    }
    let mismatched_cache: bool = query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM memory_contexts AS membership
             JOIN contexts AS context_row ON context_row.id = membership.context_id
             JOIN memory_metadata AS metadata ON metadata.memory_id = membership.memory_id
             WHERE membership.ordinal = 0
               AND NULLIF(trim(metadata.scope_key), '') IS NOT NULL
               AND metadata.scope_key <> context_row.context_key
         )",
    )
    .fetch_one(pool)
    .await?;
    if mismatched_cache {
        return Err(StoreError::Conflict(
            "PostgreSQL memory_metadata.scope_key must equal the ordinal-zero governed context key".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn postgres_runtime_indexes_compatible(pool: &PgPool, current_schema_only: bool, allow_absent: bool) -> Result<bool, SqlxError> {
    let mut connection = pool.acquire().await?;
    postgres_runtime_indexes_compatible_connection(&mut connection, current_schema_only, allow_absent).await
}

#[expect(
    clippy::too_many_lines,
    reason = "the PostgreSQL index manifest is intentionally centralized so access methods, keys, predicates, and definitions stay auditable"
)]
async fn postgres_runtime_indexes_compatible_connection(connection: &mut PgConnection, current_schema_only: bool, allow_absent: bool) -> Result<bool, SqlxError> {
    let canonical: bool = query_scalar(
        r#"SELECT COALESCE(bool_and(
            ($2 AND indexes.oid IS NULL) OR COALESCE(
                indexes.relkind = 'i'
                AND index_data.indrelid = to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), required.table_name) ELSE required.table_name END)
                AND index_data.indisvalid
                AND index_data.indisready
                AND index_data.indislive
                AND NOT index_data.indisunique
                AND NOT index_data.indisprimary
                AND NOT index_data.indisexclusion
                AND access_method.amname = CASE
                    WHEN required.name IN ('idx_memories_tags_gin', 'idx_memories_content_fts') THEN 'gin'
                    ELSE 'btree'
                END
                AND index_data.indnkeyatts = required.key_count
                AND (required.expected_keys IS NULL OR (
                    SELECT string_agg(regexp_replace(lower(pg_get_indexdef(indexes.oid, key_number, TRUE)), '[[:space:]]', '', 'g'), ',' ORDER BY key_number)
                    FROM generate_series(1, required.key_count) AS key_number
                ) = required.expected_keys)
                AND position(required.definition_fragment IN lower(pg_get_indexdef(indexes.oid))) > 0
                AND ((required.predicate IS NULL AND index_data.indpred IS NULL)
                     OR regexp_replace(lower(pg_get_expr(index_data.indpred, index_data.indrelid, TRUE)), '[()[:space:]]', '', 'g') = required.predicate)
            , FALSE)
        ), FALSE)
        FROM (VALUES
            ('idx_memories_created_at', 'memories', 1, 'created_at', 'created_at desc', NULL),
            ('idx_memories_expires_at', 'memories', 1, 'expires_at', 'expires_at', 'expires_atisnotnull'),
            ('idx_memories_has_embedding', 'memories', 1, 'has_embedding', 'has_embedding', NULL),
            ('idx_memories_memory_type', 'memories', 1, 'memory_type', 'memory_type', NULL),
            ('idx_memories_superseded_by', 'memories', 1, 'superseded_by', 'superseded_by', 'superseded_byisnotnull'),
            ('idx_memories_tags_gin', 'memories', 1, 'tags', 'using gin (tags)', NULL),
            ('idx_memories_source_agent', 'memories', 1, '(provenance->>''source_agent''::text)', 'source_agent', NULL),
            ('idx_memories_source_conversation', 'memories', 1, '(provenance->>''source_conversation''::text)', 'source_conversation', NULL),
            ('idx_memories_origin_conversation', 'memories', 1, '(provenance->>''origin_conversation''::text)', 'origin_conversation', NULL),
            ('idx_memories_effective_origin_conversation', 'memories', 1, 'coalesce(provenance->>''origin_conversation''::text,provenance->>''source_conversation''::text)', 'coalesce', NULL),
            ('idx_memories_access_type', 'memories', 1, '(access_policy->>''type''::text)', 'access_policy', NULL),
            ('idx_memories_content_fts', 'memories', 1, 'to_tsvector(''simple''::regconfig,content)', 'to_tsvector', NULL),
            ('idx_memories_embedding_claim', 'memories', 4, 'has_embedding,embedding_claimed_at,created_at,id', 'embedding_claimed_at', 'has_embedding=false'),
            ('idx_memory_entities_entity', 'memory_entities', 1, 'entity', '(entity)', NULL),
            ('idx_memory_entities_entity_type', 'memory_entities', 1, 'entity_type', 'entity_type', NULL),
            ('idx_audit_log_memory_id', 'memory_audit_log', 1, 'memory_id', 'memory_id', NULL),
            ('idx_audit_log_timestamp', 'memory_audit_log', 1, '"timestamp"', '"timestamp" desc', NULL),
            ('idx_memory_tombstones_deleted_at', 'memory_tombstones', 1, 'deleted_at', 'deleted_at desc', NULL),
            ('idx_memory_metadata_scope_key', 'memory_metadata', 1, 'scope_key', 'scope_key', NULL),
            ('idx_contexts_owner_kind_key', 'contexts', 3, 'owner_principal,kind,normalized_key', 'normalized_key', NULL),
            ('idx_contexts_kind_key', 'contexts', 3, 'kind,normalized_key,id', 'normalized_key', NULL),
            ('idx_contexts_key', 'contexts', 2, 'normalized_key,id', 'normalized_key', NULL),
            ('idx_contexts_parent', 'contexts', 1, 'parent_id', 'parent_id', 'parent_idisnotnull'),
            ('idx_contexts_lifecycle', 'contexts', 2, 'lifecycle,kind', 'lifecycle', NULL),
            ('idx_context_aliases_lookup', 'context_aliases', 2, 'normalized_alias,context_id', 'normalized_alias', NULL),
            ('idx_context_identities_lookup', 'context_identities', 5, 'owner_principal,kind,scheme,namespace,fingerprint', 'fingerprint', NULL),
            ('idx_context_identities_exact', 'context_identities', 5, 'kind,scheme,namespace,fingerprint,context_id', 'fingerprint', NULL),
            ('idx_context_resolver_hints_lookup', 'context_resolver_hints', 2, 'normalized_hint,context_id', 'normalized_hint', NULL),
            ('idx_context_grants_principal', 'context_grants', 2, 'grantee_principal,context_id', 'grantee_principal', NULL),
            ('idx_context_relations_reverse', 'context_relations', 3, 'to_context_id,relation,from_context_id', 'to_context_id', NULL),
            ('idx_memory_contexts_memory', 'memory_contexts', 3, 'memory_id,ordinal,context_id', 'ordinal', NULL),
            ('idx_memory_contexts_context', 'memory_contexts', 2, 'context_id,memory_id', 'context_id', NULL),
            ('idx_context_audit_context', 'context_audit_events', 2, 'context_id,"timestamp"', 'context_id', 'context_idisnotnull'),
            ('idx_context_audit_memory', 'context_audit_events', 2, 'memory_id,"timestamp"', 'memory_id', 'memory_idisnotnull'),
            ('idx_context_audit_timestamp', 'context_audit_events', 1, '"timestamp"', '"timestamp" desc', NULL)
        ) AS required(name, table_name, key_count, expected_keys, definition_fragment, predicate)
        LEFT JOIN pg_class AS managed_table ON managed_table.oid = to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), required.table_name) ELSE required.table_name END)
        LEFT JOIN pg_namespace AS target_namespace ON target_namespace.nspname = current_schema()
        LEFT JOIN pg_class AS indexes
          ON indexes.relnamespace = COALESCE(managed_table.relnamespace, target_namespace.oid)
         AND indexes.relname = required.name
        LEFT JOIN pg_am AS access_method ON access_method.oid = indexes.relam
        LEFT JOIN pg_index AS index_data ON index_data.indexrelid = indexes.oid"#,
    )
    .bind(current_schema_only)
    .bind(allow_absent)
    .fetch_one(&mut *connection)
    .await?;
    if !canonical {
        return Ok(false);
    }
    postgres_restrictive_indexes_compatible_connection(connection, current_schema_only).await
}

#[expect(clippy::too_many_lines, reason = "restrictive-index safety checks are kept in one catalog query and one validation loop")]
async fn postgres_restrictive_indexes_compatible_connection(connection: &mut PgConnection, current_schema_only: bool) -> Result<bool, SqlxError> {
    let rows = query(
        "SELECT managed.relname AS table_name,
                restrictive.indisexclusion,
                COALESCE((to_jsonb(restrictive)->>'indnullsnotdistinct')::boolean, FALSE) AS indnullsnotdistinct,
                restrictive.indpred IS NOT NULL AS has_predicate,
                EXISTS(
                    SELECT 1
                    FROM unnest(
                        restrictive.indkey::smallint[],
                        restrictive.indclass::oid[],
                        restrictive.indcollation::oid[]
                    ) WITH ORDINALITY AS key(attnum, opclass_oid, collation_oid, ordinal)
                    LEFT JOIN pg_attribute AS key_attribute
                      ON key_attribute.attrelid = restrictive.indrelid
                     AND key_attribute.attnum = key.attnum
                    LEFT JOIN pg_opclass AS key_opclass ON key_opclass.oid = key.opclass_oid
                    WHERE key.ordinal <= restrictive.indnkeyatts
                      AND (
                          key_attribute.attnum IS NULL
                          OR key.collation_oid <> key_attribute.attcollation
                          OR NOT COALESCE(key_opclass.opcdefault, FALSE)
                      )
                ) AS has_nondefault_key_semantics,
                restrictive.indnkeyatts::bigint AS key_column_count,
                ARRAY(
                    SELECT regexp_replace(lower(pg_get_indexdef(restrictive.indexrelid, key_number, TRUE)), '[[:space:]]', '', 'g')
                    FROM generate_series(1, restrictive.indnkeyatts) AS key_number
                    ORDER BY regexp_replace(lower(pg_get_indexdef(restrictive.indexrelid, key_number, TRUE)), '[[:space:]]', '', 'g')
                )::text[] AS key_definitions
         FROM pg_index AS restrictive
         JOIN pg_class AS managed ON managed.oid = restrictive.indrelid
         WHERE restrictive.indrelid = ANY(ARRAY[
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'localhold_migrations') ELSE 'localhold_migrations' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memories') ELSE 'memories' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_entities') ELSE 'memory_entities' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_embeddings') ELSE 'memory_embeddings' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_audit_log') ELSE 'memory_audit_log' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_tombstones') ELSE 'memory_tombstones' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_kinds') ELSE 'context_kinds' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'contexts') ELSE 'contexts' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_aliases') ELSE 'context_aliases' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_identities') ELSE 'context_identities' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_resolver_hints') ELSE 'context_resolver_hints' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_grants') ELSE 'context_grants' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_relations') ELSE 'context_relations' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_contexts') ELSE 'memory_contexts' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_kind_policies') ELSE 'context_kind_policies' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_anchor_overrides') ELSE 'context_anchor_overrides' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_audit_events') ELSE 'context_audit_events' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_metadata') ELSE 'memory_metadata' END),
             to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'embedding_profile') ELSE 'embedding_profile' END)
         ])
           AND restrictive.indisvalid
           AND restrictive.indisready
           AND restrictive.indislive
           AND (restrictive.indisunique OR restrictive.indisexclusion)",
    )
    .bind(current_schema_only)
    .fetch_all(&mut *connection)
    .await?;

    for row in rows {
        let table: String = row.try_get("table_name")?;
        let exclusion: bool = row.try_get("indisexclusion")?;
        let nulls_not_distinct: bool = row.try_get("indnullsnotdistinct")?;
        let has_predicate: bool = row.try_get("has_predicate")?;
        let has_nondefault_key_semantics: bool = row.try_get("has_nondefault_key_semantics")?;
        let key_column_count: i64 = row.try_get("key_column_count")?;
        let key_definitions: Vec<String> = row.try_get("key_definitions")?;
        let allowed = !exclusion
            && !nulls_not_distinct
            && !has_predicate
            && !has_nondefault_key_semantics
            && usize::try_from(key_column_count).ok() == Some(key_definitions.len())
            && POSTGRES_REQUIRED_KEYS
                .iter()
                .any(|expectation| expectation.table == table && sorted_key_columns(expectation.columns) == key_definitions);
        if !allowed {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostgresRelationshipValidationMode {
    Runtime,
    Migration,
    PublishedMigration,
}

pub(crate) async fn validate_postgres_runtime_relationships(pool: &PgPool, current_schema_only: bool) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    validate_postgres_runtime_relationships_mode_connection(&mut connection, current_schema_only, PostgresRelationshipValidationMode::Runtime).await
}

pub(crate) async fn validate_postgres_runtime_relationships_before_migration(pool: &PgPool, current_schema_only: bool) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    validate_postgres_runtime_relationships_mode_connection(&mut connection, current_schema_only, PostgresRelationshipValidationMode::Migration).await
}

pub(crate) async fn validate_postgres_runtime_relationships_before_migration_connection(
    connection: &mut PgConnection,
    current_schema_only: bool,
    allow_published_metadata: bool,
) -> Result<(), StoreError> {
    let mode = if allow_published_metadata {
        PostgresRelationshipValidationMode::PublishedMigration
    } else {
        PostgresRelationshipValidationMode::Migration
    };
    validate_postgres_runtime_relationships_mode_connection(connection, current_schema_only, mode).await
}

async fn validate_postgres_runtime_relationships_mode_connection(
    connection: &mut PgConnection,
    current_schema_only: bool,
    mode: PostgresRelationshipValidationMode,
) -> Result<(), StoreError> {
    let allow_legacy_audit_fk = mode != PostgresRelationshipValidationMode::Runtime;
    let allow_published_metadata = mode == PostgresRelationshipValidationMode::PublishedMigration;
    let allow_absent_child_tables = mode != PostgresRelationshipValidationMode::Runtime;
    let mut required_foreign_key_count = 0_i64;
    for expected in POSTGRES_REQUIRED_FOREIGN_KEYS {
        let mut expectation = *expected;
        if allow_published_metadata && expectation.child_table == "memory_metadata" {
            expectation.child_table = "memory_v2_metadata";
        }
        if allow_absent_child_tables && !postgres_table_exists_connection(connection, expectation.child_table, current_schema_only).await? {
            continue;
        }
        required_foreign_key_count = required_foreign_key_count.saturating_add(1_i64);
        if !postgres_foreign_key_matches_connection(connection, expectation, current_schema_only).await? {
            return Err(StoreError::Conflict(format!(
                "PostgreSQL relational constraint {}.{} does not match runtime cascade requirements",
                expectation.child_table, expectation.child_column
            )));
        }
    }
    let audit_foreign_key_count = postgres_foreign_key_count_to_connection(connection, "memory_audit_log", "memories", current_schema_only).await?;
    if !allow_legacy_audit_fk && audit_foreign_key_count != 0_i64 {
        return Err(StoreError::Conflict(
            "PostgreSQL relational constraint on memory_audit_log is incompatible with retained history requirements".into(),
        ));
    }
    if postgres_foreign_key_count_to_connection(connection, "memory_tombstones", "memories", current_schema_only).await? != 0_i64 {
        return Err(StoreError::Conflict(
            "PostgreSQL relational constraint on memory_tombstones is incompatible with retained history requirements".into(),
        ));
    }
    let metadata_table = if allow_published_metadata { "memory_v2_metadata" } else { "memory_metadata" };
    let foreign_key_count = postgres_managed_foreign_key_count_connection(connection, current_schema_only, metadata_table).await?;
    let expected_count = required_foreign_key_count.saturating_add(if allow_legacy_audit_fk { audit_foreign_key_count } else { 0_i64 });
    if foreign_key_count != expected_count {
        return Err(StoreError::Conflict("PostgreSQL managed schema contains unexpected foreign key constraints".into()));
    }
    Ok(())
}

async fn postgres_managed_foreign_key_count_connection(connection: &mut PgConnection, current_schema_only: bool, metadata_table: &str) -> Result<i64, StoreError> {
    query_scalar(
        "SELECT COUNT(*)
         FROM pg_constraint AS foreign_key
         WHERE foreign_key.contype = 'f'
           AND foreign_key.conrelid = ANY(ARRAY[
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memories') ELSE 'memories' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_entities') ELSE 'memory_entities' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_embeddings') ELSE 'memory_embeddings' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_audit_log') ELSE 'memory_audit_log' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_tombstones') ELSE 'memory_tombstones' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_kinds') ELSE 'context_kinds' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'contexts') ELSE 'contexts' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_aliases') ELSE 'context_aliases' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_identities') ELSE 'context_identities' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_resolver_hints') ELSE 'context_resolver_hints' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_grants') ELSE 'context_grants' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_relations') ELSE 'context_relations' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_contexts') ELSE 'memory_contexts' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_kind_policies') ELSE 'context_kind_policies' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_anchor_overrides') ELSE 'context_anchor_overrides' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'context_audit_events') ELSE 'context_audit_events' END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), $2) ELSE $2 END),
               to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'embedding_profile') ELSE 'embedding_profile' END)
           ])",
    )
    .bind(current_schema_only)
    .bind(metadata_table)
    .fetch_one(&mut *connection)
    .await
    .map_err(StoreError::from)
}

async fn postgres_foreign_key_matches_connection(connection: &mut PgConnection, expectation: PostgresForeignKeyExpectation, current_schema_only: bool) -> Result<bool, StoreError> {
    query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM pg_constraint AS foreign_key
            WHERE foreign_key.conrelid = to_regclass(CASE WHEN $6 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)
              AND foreign_key.confrelid = to_regclass(CASE WHEN $6 THEN format('%I.%I', current_schema(), $3) ELSE $3 END)
              AND foreign_key.contype = 'f'
              AND foreign_key.convalidated
              AND foreign_key.confupdtype = 'a'
              AND foreign_key.confdeltype::text = $5
              AND foreign_key.confmatchtype = 's'
              AND NOT foreign_key.condeferrable
              AND NOT foreign_key.condeferred
              AND foreign_key.conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = foreign_key.conrelid AND attname = $2 AND NOT attisdropped)]::smallint[]
              AND foreign_key.confkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = foreign_key.confrelid AND attname = $4 AND NOT attisdropped)]::smallint[]
        )",
    )
    .bind(expectation.child_table)
    .bind(expectation.child_column)
    .bind(expectation.parent_table)
    .bind(expectation.parent_column)
    .bind(expectation.delete_action)
    .bind(current_schema_only)
    .fetch_one(&mut *connection)
    .await
    .map_err(StoreError::from)
}

async fn postgres_foreign_key_count_to_connection(connection: &mut PgConnection, child_table: &str, parent_table: &str, current_schema_only: bool) -> Result<i64, StoreError> {
    query_scalar(
        "SELECT COUNT(*)
         FROM pg_constraint AS foreign_key
         WHERE foreign_key.conrelid = to_regclass(CASE WHEN $3 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)
           AND foreign_key.confrelid = to_regclass(CASE WHEN $3 THEN format('%I.%I', current_schema(), $2) ELSE $2 END)
           AND foreign_key.contype = 'f'",
    )
    .bind(child_table)
    .bind(parent_table)
    .bind(current_schema_only)
    .fetch_one(&mut *connection)
    .await
    .map_err(StoreError::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentPostgresVectorPolicy {
    ValidateDimensions(usize),
    Rebuild,
}

/// Validate every managed `PostgreSQL` table that is already present while
/// allowing absent tables and known migration-added columns.
pub(crate) async fn validate_present_postgres_schema(
    pool: &PgPool,
    embedding_dimensions: usize,
    current_schema_only: bool,
    include_migration_metadata: bool,
) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    validate_present_postgres_schema_connection(
        &mut connection,
        current_schema_only,
        include_migration_metadata,
        false,
        PresentPostgresVectorPolicy::ValidateDimensions(embedding_dimensions),
    )
    .await
}

/// Validate all present managed objects while allowing the separately
/// validated metadata table written by published beta releases.
pub(crate) async fn validate_present_postgres_schema_for_published_upgrade(
    pool: &PgPool,
    embedding_dimensions: usize,
    current_schema_only: bool,
    include_migration_metadata: bool,
) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    validate_present_postgres_schema_connection(
        &mut connection,
        current_schema_only,
        include_migration_metadata,
        true,
        PresentPostgresVectorPolicy::ValidateDimensions(embedding_dimensions),
    )
    .await
}

pub(crate) async fn validate_present_postgres_schema_connection(
    connection: &mut PgConnection,
    current_schema_only: bool,
    include_migration_metadata: bool,
    allow_published_metadata: bool,
    vector_policy: PresentPostgresVectorPolicy,
) -> Result<(), StoreError> {
    if !allow_published_metadata {
        reject_retired_postgres_schema_connection(connection, current_schema_only).await?;
    }
    for table in std::iter::once(&POSTGRES_MIGRATIONS_TABLE)
        .filter(|_| include_migration_metadata)
        .chain(POSTGRES_USER_TABLES.iter())
    {
        if !postgres_table_exists_connection(connection, table, current_schema_only).await? {
            continue;
        }
        validate_postgres_table_kind_connection(connection, table, current_schema_only).await?;
        for expectation in POSTGRES_REQUIRED_COLUMNS.iter().filter(|expectation| expectation.table == *table) {
            if allow_published_metadata && expectation.table == "memories" && expectation.column == "record_revision" {
                validate_optional_postgres_column_type_connection(connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
                continue;
            }
            validate_postgres_column_type_connection(connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
        }
        for expectation in POSTGRES_OPTIONAL_COLUMNS.iter().filter(|expectation| expectation.table == *table) {
            validate_optional_postgres_column_type_connection(connection, expectation.table, expectation.column, expectation.formatted_type, current_schema_only).await?;
        }
        validate_postgres_table_contracts_inner_connection(connection, table, current_schema_only, allow_published_metadata).await?;
        validate_postgres_required_keys_connection(connection, table, current_schema_only).await?;
    }
    if postgres_table_exists_connection(connection, "memory_embeddings", current_schema_only).await? {
        match vector_policy {
            PresentPostgresVectorPolicy::ValidateDimensions(embedding_dimensions) => {
                validate_postgres_column_type_connection(
                    connection,
                    "memory_embeddings",
                    "embedding",
                    &format!("vector({embedding_dimensions})"),
                    current_schema_only,
                )
                .await?;
            }
            PresentPostgresVectorPolicy::Rebuild => {
                let actual = postgres_column_type_connection(connection, "memory_embeddings", "embedding", current_schema_only).await?;
                if actual.as_deref().and_then(parse_pgvector_dimensions).is_none() {
                    return Err(StoreError::Conflict(format!(
                        "PostgreSQL target column memory_embeddings.embedding has type {}, expected vector(n)",
                        actual.as_deref().unwrap_or("<missing>")
                    )));
                }
                validate_postgres_not_null_column_connection(connection, "memory_embeddings", "embedding", current_schema_only).await?;
            }
        }
    }
    if !postgres_runtime_indexes_compatible_connection(connection, current_schema_only, true).await? {
        return Err(StoreError::Conflict(
            "PostgreSQL managed schema indexes do not match runtime requirements; an existing index is incompatible and startup cannot safely replace it".into(),
        ));
    }
    Ok(())
}

pub(crate) fn reject_retired_sqlite_schema(conn: &Connection) -> Result<(), StoreError> {
    if sqlite_schema_sql(conn, "table", RETIRED_METADATA_TABLE)?.is_some() {
        return Err(StoreError::Conflict(format!(
            "SQLite contains unsupported prior-iteration table {RETIRED_METADATA_TABLE}; back up and reset the database before using this LocalHold version"
        )));
    }
    Ok(())
}

pub(crate) async fn reject_retired_postgres_schema(pool: &PgPool, current_schema_only: bool) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    reject_retired_postgres_schema_connection(&mut connection, current_schema_only).await
}

async fn reject_retired_postgres_schema_connection(connection: &mut PgConnection, current_schema_only: bool) -> Result<(), StoreError> {
    if postgres_table_exists_connection(connection, RETIRED_METADATA_TABLE, current_schema_only).await? {
        return Err(StoreError::Conflict(format!(
            "PostgreSQL contains unsupported prior-iteration table {RETIRED_METADATA_TABLE}; back up and reset the database before using this LocalHold version"
        )));
    }
    Ok(())
}

async fn validate_postgres_required_keys_connection(connection: &mut PgConnection, table: &'static str, current_schema_only: bool) -> Result<(), StoreError> {
    for expectation in POSTGRES_REQUIRED_KEYS.iter().filter(|expectation| expectation.table == table) {
        validate_postgres_unique_or_primary_key_connection(connection, expectation.table, expectation.columns, current_schema_only).await?;
    }
    Ok(())
}

async fn validate_postgres_table_contracts_connection(connection: &mut PgConnection, table: &'static str, current_schema_only: bool) -> Result<(), StoreError> {
    validate_postgres_table_contracts_inner_connection(connection, table, current_schema_only, false).await
}

async fn validate_postgres_table_contracts_inner_connection(
    connection: &mut PgConnection,
    table: &'static str,
    current_schema_only: bool,
    allow_missing_record_revision: bool,
) -> Result<(), StoreError> {
    for expectation in POSTGRES_REQUIRED_COLUMNS
        .iter()
        .chain(POSTGRES_OPTIONAL_COLUMNS.iter())
        .filter(|expectation| expectation.table == table)
    {
        let actual = postgres_column_not_null_connection(connection, table, expectation.column, current_schema_only).await?;
        let Some(actual) = actual else {
            continue;
        };
        let expected = !POSTGRES_NULLABLE_COLUMNS.contains(&(table, expectation.column));
        if actual != expected {
            return Err(StoreError::Conflict(format!(
                "PostgreSQL target column {table}.{} has incompatible nullability; expected {}",
                expectation.column,
                if expected { "NOT NULL" } else { "nullable" }
            )));
        }
    }
    if table == "memory_embeddings" {
        validate_postgres_not_null_column_connection(connection, table, "embedding", current_schema_only).await?;
    }
    for (_, column, expected_default) in POSTGRES_REQUIRED_DEFAULTS.iter().filter(|(expected_table, ..)| *expected_table == table) {
        if allow_missing_record_revision
            && table == "memories"
            && *column == "record_revision"
            && postgres_column_not_null_connection(connection, table, column, current_schema_only).await?.is_none()
        {
            continue;
        }
        validate_postgres_default_contract_connection(connection, table, column, expected_default, current_schema_only).await?;
    }
    if table == "memories" {
        validate_postgres_embedding_revision_default_connection(connection, current_schema_only).await?;
    }
    if matches!(table, "memory_audit_log" | "context_audit_events") {
        validate_postgres_serial_default_connection(connection, table, "id", current_schema_only).await?;
    }
    validate_postgres_check_constraints_connection(connection, table, current_schema_only).await?;
    Ok(())
}

async fn validate_postgres_check_constraints_connection(connection: &mut PgConnection, table: &'static str, current_schema_only: bool) -> Result<(), StoreError> {
    let rows = query(
        "SELECT pg_get_expr(check_constraint.conbin, check_constraint.conrelid, TRUE) AS expression,
                check_constraint.convalidated,
                check_constraint.connoinherit
         FROM pg_constraint AS check_constraint
         WHERE check_constraint.conrelid = to_regclass(
                   CASE WHEN $2 THEN format('%I.%I', current_schema(), $1) ELSE $1 END
               )
           AND check_constraint.contype = 'c'",
    )
    .bind(table)
    .bind(current_schema_only)
    .fetch_all(&mut *connection)
    .await?;
    let mut actual = BTreeSet::new();
    for row in rows {
        let validated: bool = row.try_get("convalidated")?;
        let no_inherit: bool = row.try_get("connoinherit")?;
        if !validated || no_inherit {
            return Err(StoreError::Conflict(format!(
                "PostgreSQL table {table} has a noncanonical CHECK constraint validation mode"
            )));
        }
        let expression: String = row.try_get("expression")?;
        let _inserted = actual.insert(normalize_postgres_check_expression(&expression));
    }
    let expected = POSTGRES_REQUIRED_CHECKS
        .iter()
        .filter(|(expected_table, _)| *expected_table == table)
        .map(|(_, expression)| normalize_postgres_check_expression(expression))
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(StoreError::Conflict(format!(
            "PostgreSQL table {table} has unexpected CHECK constraints: actual {actual:?}, expected {expected:?}"
        )));
    }
    Ok(())
}

fn normalize_postgres_check_expression(value: &str) -> String {
    value.chars().filter(|character| !character.is_ascii_whitespace()).flat_map(char::to_lowercase).collect()
}

async fn postgres_column_not_null_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    current_schema_only: bool,
) -> Result<Option<bool>, StoreError> {
    query_scalar(
        "SELECT attribute.attnotnull FROM pg_attribute AS attribute WHERE attribute.attrelid = to_regclass(CASE WHEN $3 THEN format('%I.%I', current_schema(), $1) ELSE $1 END) AND attribute.attname = $2 AND NOT attribute.attisdropped",
    )
    .bind(table)
    .bind(column)
    .bind(current_schema_only)
    .fetch_optional(&mut *connection)
    .await
    .map_err(StoreError::from)
}

async fn validate_postgres_not_null_column_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    current_schema_only: bool,
) -> Result<(), StoreError> {
    if postgres_column_not_null_connection(connection, table, column, current_schema_only).await? == Some(true) {
        Ok(())
    } else {
        Err(StoreError::Conflict(format!("PostgreSQL target column {table}.{column} must be NOT NULL")))
    }
}

async fn validate_postgres_serial_default_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    current_schema_only: bool,
) -> Result<(), StoreError> {
    let sequence_name: Option<String> = query_scalar(
        "SELECT format('%I.%I', sequence_namespace.nspname, sequence.relname)
            FROM pg_attribute AS attribute
            JOIN pg_attrdef AS definition ON definition.adrelid = attribute.attrelid AND definition.adnum = attribute.attnum
            JOIN pg_depend AS default_dependency ON default_dependency.classid = 'pg_attrdef'::regclass AND default_dependency.objid = definition.oid AND default_dependency.refclassid = 'pg_class'::regclass
            JOIN pg_class AS sequence ON sequence.oid = default_dependency.refobjid AND sequence.relkind = 'S'
            JOIN pg_namespace AS sequence_namespace ON sequence_namespace.oid = sequence.relnamespace
            JOIN pg_sequence AS sequence_config ON sequence_config.seqrelid = sequence.oid
            JOIN pg_depend AS ownership ON ownership.classid = 'pg_class'::regclass AND ownership.objid = sequence.oid AND ownership.refclassid = 'pg_class'::regclass AND ownership.refobjid = attribute.attrelid AND ownership.refobjsubid = attribute.attnum AND ownership.deptype IN ('a', 'i')
            WHERE attribute.attrelid = to_regclass(CASE WHEN $3 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)
              AND attribute.attname = $2
              AND NOT attribute.attisdropped
              AND pg_get_expr(definition.adbin, definition.adrelid) LIKE 'nextval(''%''::regclass)'
              AND sequence_config.seqtypid = 'bigint'::regtype
              AND sequence_config.seqstart = 1
              AND sequence_config.seqincrement = 1
              AND sequence_config.seqmax = 9223372036854775807
              AND sequence_config.seqmin = 1
              AND sequence_config.seqcache = 1
              AND NOT sequence_config.seqcycle
            LIMIT 1",
    )
    .bind(table)
    .bind(column)
    .bind(current_schema_only)
    .fetch_optional(&mut *connection)
    .await?;
    let Some(sequence_name) = sequence_name else {
        return Err(StoreError::Conflict(format!(
            "PostgreSQL target column {table}.{column} must default from its owned sequence"
        )));
    };
    let sequence_state = query(AssertSqlSafe(format!("SELECT last_value, is_called FROM {sequence_name}")))
        .fetch_one(&mut *connection)
        .await?;
    let last_value: i64 = sequence_state.try_get("last_value")?;
    let is_called: bool = sequence_state.try_get("is_called")?;
    let next_value = if is_called { last_value.checked_add(1) } else { Some(last_value) };
    let qualified_table = postgres_qualified_relation_name_connection(connection, table, current_schema_only).await?;
    let max_id: Option<i64> = query_scalar(AssertSqlSafe(format!("SELECT MAX({column}) FROM {qualified_table}")))
        .fetch_one(&mut *connection)
        .await?;
    if next_value.is_some_and(|next| max_id.is_none_or(|maximum| next > maximum)) {
        Ok(())
    } else {
        Err(StoreError::Conflict(format!(
            "PostgreSQL target sequence for {table}.{column} will not generate an unused next value"
        )))
    }
}

async fn postgres_qualified_relation_name_connection(connection: &mut PgConnection, table: &'static str, current_schema_only: bool) -> Result<String, StoreError> {
    query_scalar(
        "SELECT format('%I.%I', namespace.nspname, relation.relname) FROM pg_class AS relation JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace WHERE relation.oid = to_regclass(CASE WHEN $2 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)",
    )
    .bind(table)
    .bind(current_schema_only)
    .fetch_one(&mut *connection)
    .await
    .map_err(StoreError::from)
}

async fn validate_postgres_default_contract_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    expected_default: &'static str,
    current_schema_only: bool,
) -> Result<(), StoreError> {
    let contract: Option<bool> = query_scalar(
        "SELECT attribute.attnotnull AND pg_get_expr(definition.adbin, definition.adrelid) = $3 FROM pg_attribute AS attribute LEFT JOIN pg_attrdef AS definition ON definition.adrelid = attribute.attrelid AND definition.adnum = attribute.attnum WHERE attribute.attrelid = to_regclass(CASE WHEN $4 THEN format('%I.%I', current_schema(), $1) ELSE $1 END) AND attribute.attname = $2 AND NOT attribute.attisdropped",
    )
    .bind(table)
    .bind(column)
    .bind(expected_default)
    .bind(current_schema_only)
    .fetch_optional(&mut *connection)
    .await?;
    if contract == Some(true) {
        Ok(())
    } else {
        Err(StoreError::Conflict(format!(
            "PostgreSQL target column {table}.{column} must be NOT NULL with DEFAULT {expected_default} because startup does not repair an existing definition"
        )))
    }
}

async fn validate_postgres_embedding_revision_default_connection(connection: &mut PgConnection, current_schema_only: bool) -> Result<(), StoreError> {
    let default: Option<String> = query_scalar(
        "SELECT pg_get_expr(definition.adbin, definition.adrelid) FROM pg_attribute AS attribute LEFT JOIN pg_attrdef AS definition ON definition.adrelid = attribute.attrelid AND definition.adnum = attribute.attnum WHERE attribute.attrelid = to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memories') ELSE 'memories' END) AND attribute.attname = 'embedding_revision' AND NOT attribute.attisdropped",
    )
    .bind(current_schema_only)
    .fetch_one(&mut *connection)
    .await?;
    let default_is_zero = default.as_deref().is_some_and(|value| {
        let normalized = value.trim().trim_matches(['(', ')']);
        normalized.strip_suffix("::bigint").unwrap_or(normalized) == "0"
    });
    if default_is_zero {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "PostgreSQL target column memories.embedding_revision must have DEFAULT 0 because startup does not repair an existing definition".into(),
        ))
    }
}

async fn postgres_table_exists(pool: &PgPool, table: &'static str, current_schema_only: bool) -> Result<bool, StoreError> {
    let mut connection = pool.acquire().await?;
    postgres_table_exists_connection(&mut connection, table, current_schema_only).await
}

async fn postgres_table_exists_connection(connection: &mut PgConnection, table: &'static str, current_schema_only: bool) -> Result<bool, StoreError> {
    query_scalar("SELECT to_regclass(CASE WHEN $2 THEN format('%I.%I', current_schema(), $1) ELSE $1 END) IS NOT NULL")
        .bind(table)
        .bind(current_schema_only)
        .fetch_one(&mut *connection)
        .await
        .map_err(StoreError::from)
}

async fn validate_postgres_table_kind_connection(connection: &mut PgConnection, table: &'static str, current_schema_only: bool) -> Result<(), StoreError> {
    let relkind: Option<String> = query_scalar("SELECT relkind::text FROM pg_class WHERE oid = to_regclass(CASE WHEN $2 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)")
        .bind(table)
        .bind(current_schema_only)
        .fetch_optional(&mut *connection)
        .await?;
    match relkind.as_deref() {
        Some("r" | "p") | None => Ok(()),
        Some(kind) => Err(StoreError::Conflict(format!("PostgreSQL target object {table} is relkind {kind}, expected a table"))),
    }
}

async fn validate_postgres_column_type_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    expected_type: &str,
    current_schema_only: bool,
) -> Result<(), StoreError> {
    let actual_type = postgres_column_type_connection(connection, table, column, current_schema_only).await?;

    match actual_type {
        Some(actual_type) if actual_type == expected_type => Ok(()),
        Some(actual_type) => Err(StoreError::Conflict(format!(
            "PostgreSQL target column {table}.{column} has type {actual_type}, expected {expected_type}"
        ))),
        None => Err(StoreError::Conflict(format!("PostgreSQL target table {table} is missing required column {column}"))),
    }
}

async fn validate_optional_postgres_column_type_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    expected_type: &str,
    current_schema_only: bool,
) -> Result<(), StoreError> {
    let actual_type = postgres_column_type_connection(connection, table, column, current_schema_only).await?;

    match actual_type {
        Some(actual_type) if actual_type == expected_type => Ok(()),
        Some(actual_type) => Err(StoreError::Conflict(format!(
            "PostgreSQL target column {table}.{column} has type {actual_type}, expected {expected_type}"
        ))),
        None => Ok(()),
    }
}

async fn postgres_column_type_connection(
    connection: &mut PgConnection,
    table: &'static str,
    column: &'static str,
    current_schema_only: bool,
) -> Result<Option<String>, StoreError> {
    query_scalar(
        "
        SELECT format_type(attribute.atttypid, attribute.atttypmod)
        FROM pg_attribute AS attribute
        WHERE attribute.attrelid = to_regclass(CASE WHEN $3 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)
          AND attribute.attname = $2
          AND NOT attribute.attisdropped
        ",
    )
    .bind(table)
    .bind(column)
    .bind(current_schema_only)
    .fetch_optional(&mut *connection)
    .await
    .map_err(StoreError::from)
}

async fn validate_postgres_unique_or_primary_key_connection(
    connection: &mut PgConnection,
    table: &'static str,
    columns: &'static [&'static str],
    current_schema_only: bool,
) -> Result<(), StoreError> {
    let expected_columns = sorted_key_columns(columns);
    let has_key: bool = query_scalar(
        "
        SELECT EXISTS (
            SELECT 1
            FROM pg_index AS index_row
            CROSS JOIN LATERAL (
                SELECT
                    ARRAY_AGG(attribute.attname ORDER BY attribute.attname)::text[] AS column_names,
                    COUNT(*) AS key_column_count,
                    COUNT(attribute.attname) AS matched_column_count
                FROM unnest(index_row.indkey) WITH ORDINALITY AS key(attnum, ordinal)
                LEFT JOIN pg_attribute AS attribute
                  ON attribute.attrelid = index_row.indrelid
                 AND attribute.attnum = key.attnum
                WHERE key.ordinal <= index_row.indnkeyatts
            ) AS key_columns
            WHERE index_row.indrelid = to_regclass(CASE WHEN $3 THEN format('%I.%I', current_schema(), $1) ELSE $1 END)
              AND index_row.indisunique
              AND index_row.indisvalid
              AND index_row.indisready
              AND index_row.indpred IS NULL
              AND key_columns.key_column_count = cardinality($2::text[])
              AND key_columns.matched_column_count = cardinality($2::text[])
              AND key_columns.column_names = $2::text[]
        )
        ",
    )
    .bind(table)
    .bind(&expected_columns)
    .bind(current_schema_only)
    .fetch_one(&mut *connection)
    .await?;

    if !has_key {
        let key = postgres_key_label(table, columns);
        return Err(StoreError::Conflict(format!("PostgreSQL target key {key} must have a primary-key or unique index")));
    }
    Ok(())
}

fn sorted_key_columns(columns: &'static [&'static str]) -> Vec<String> {
    let mut sorted = columns.iter().map(|column| (*column).to_owned()).collect::<Vec<_>>();
    sorted.sort();
    sorted
}

fn postgres_key_label(table: &'static str, columns: &'static [&'static str]) -> String {
    match columns {
        [column] => format!("{table}.{column}"),
        _ => format!("{table}({})", columns.join(", ")),
    }
}

async fn postgres_counts_existing(pool: &PgPool) -> Result<MigrationTableCounts, StoreError> {
    Ok(MigrationTableCounts {
        memories: postgres_count_existing(pool, "memories").await?,
        entities: postgres_count_existing(pool, "memory_entities").await?,
        embeddings: postgres_count_existing(pool, "memory_embeddings").await?,
        audit_entries: postgres_count_existing(pool, "memory_audit_log").await?,
        tombstones: postgres_count_existing(pool, "memory_tombstones").await?,
        context_kinds: postgres_count_existing(pool, "context_kinds").await?,
        contexts: postgres_count_existing(pool, "contexts").await?,
        context_aliases: postgres_count_existing(pool, "context_aliases").await?,
        context_identities: postgres_count_existing(pool, "context_identities").await?,
        context_hints: postgres_count_existing(pool, "context_resolver_hints").await?,
        context_grants: postgres_count_existing(pool, "context_grants").await?,
        context_relations: postgres_count_existing(pool, "context_relations").await?,
        memory_contexts: postgres_count_existing(pool, "memory_contexts").await?,
        context_policies: postgres_count_existing(pool, "context_kind_policies").await?,
        context_anchor_overrides: postgres_count_existing(pool, "context_anchor_overrides").await?,
        context_audit_events: postgres_count_existing(pool, "context_audit_events").await?,
        metadata: postgres_count_existing(pool, "memory_metadata").await?,
        embedding_profiles: postgres_count_existing(pool, "embedding_profile").await?,
    })
}

async fn postgres_count_existing(pool: &PgPool, table: &'static str) -> Result<u64, StoreError> {
    let exists = postgres_table_exists(pool, table, true).await?;
    if !exists {
        return Ok(0);
    }
    let raw: i64 = query_scalar::<_, i64>(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}"))).fetch_one(pool).await?;
    u64::try_from(raw).map_err(|e| StoreError::Serialization(Box::new(e)))
}

async fn check_existing_postgres_vector_dimensions(pool: &PgPool, embedding_dimensions: usize) -> Result<(), StoreError> {
    let existing_type: Option<String> = query_scalar(
        "
        SELECT format_type(attribute.atttypid, attribute.atttypmod)
        FROM pg_attribute AS attribute
        WHERE attribute.attrelid = to_regclass(format('%I.%I', current_schema(), 'memory_embeddings'))
          AND attribute.attname = 'embedding'
          AND NOT attribute.attisdropped
        ",
    )
    .fetch_optional(pool)
    .await?;

    let Some(existing_type) = existing_type else {
        return Ok(());
    };
    let Some(existing_dimensions) = parse_pgvector_dimensions(&existing_type) else {
        return Err(StoreError::Conflict(format!(
            "existing memory_embeddings.embedding type is {existing_type}, expected vector({embedding_dimensions})"
        )));
    };
    if existing_dimensions != embedding_dimensions {
        return Err(StoreError::Conflict(format!(
            "existing memory_embeddings table has {existing_dimensions} dimensions but migration specifies {embedding_dimensions}"
        )));
    }
    Ok(())
}

fn parse_pgvector_dimensions(formatted_type: &str) -> Option<usize> {
    let inner = formatted_type.strip_prefix("vector(")?.strip_suffix(')')?;
    inner.parse().ok()
}

async fn postgres_counts_tx(tx: &mut Transaction<'_, Postgres>) -> Result<MigrationTableCounts, StoreError> {
    Ok(MigrationTableCounts {
        memories: postgres_count_tx(tx, "memories").await?,
        entities: postgres_count_tx(tx, "memory_entities").await?,
        embeddings: postgres_count_tx(tx, "memory_embeddings").await?,
        audit_entries: postgres_count_tx(tx, "memory_audit_log").await?,
        tombstones: postgres_count_tx(tx, "memory_tombstones").await?,
        context_kinds: postgres_count_tx(tx, "context_kinds").await?,
        contexts: postgres_count_tx(tx, "contexts").await?,
        context_aliases: postgres_count_tx(tx, "context_aliases").await?,
        context_identities: postgres_count_tx(tx, "context_identities").await?,
        context_hints: postgres_count_tx(tx, "context_resolver_hints").await?,
        context_grants: postgres_count_tx(tx, "context_grants").await?,
        context_relations: postgres_count_tx(tx, "context_relations").await?,
        memory_contexts: postgres_count_tx(tx, "memory_contexts").await?,
        context_policies: postgres_count_tx(tx, "context_kind_policies").await?,
        context_anchor_overrides: postgres_count_tx(tx, "context_anchor_overrides").await?,
        context_audit_events: postgres_count_tx(tx, "context_audit_events").await?,
        metadata: postgres_count_tx(tx, "memory_metadata").await?,
        embedding_profiles: postgres_count_tx(tx, "embedding_profile").await?,
    })
}

async fn lock_postgres_user_tables(tx: &mut Transaction<'_, Postgres>) -> Result<(), StoreError> {
    let _result = query("SELECT set_config('lock_timeout', $1, true)").bind(POSTGRES_LOCK_TIMEOUT).execute(&mut **tx).await?;
    let _result = query(
        "
        LOCK TABLE
            memories,
            memory_entities,
            memory_embeddings,
            memory_audit_log,
            memory_tombstones,
            context_kinds,
            contexts,
            context_aliases,
            context_identities,
            context_resolver_hints,
            context_grants,
            context_relations,
            memory_contexts,
            context_kind_policies,
            context_anchor_overrides,
            context_audit_events,
            memory_metadata,
            embedding_profile
        IN ACCESS EXCLUSIVE MODE
        ",
    )
    .execute(&mut **tx)
    .await
    .map_err(postgres_lock_error)?;
    Ok(())
}

#[expect(clippy::wildcard_enum_match_arm, reason = "non-lock SQLx errors should preserve StoreError conversion")]
fn postgres_lock_error(error: SqlxError) -> StoreError {
    match &error {
        SqlxError::Database(database_error) if database_error.code().as_deref() == Some("55P03") => {
            StoreError::Conflict(format!("timed out waiting for PostgreSQL target table locks after {POSTGRES_LOCK_TIMEOUT}"))
        }
        _ => StoreError::from(error),
    }
}

async fn postgres_count_tx(tx: &mut Transaction<'_, Postgres>, table: &'static str) -> Result<u64, StoreError> {
    let raw: i64 = query_scalar::<_, i64>(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}"))).fetch_one(&mut **tx).await?;
    u64::try_from(raw).map_err(|e| StoreError::Serialization(Box::new(e)))
}

async fn import_postgres(tx: &mut Transaction<'_, Postgres>, source: &MigrationSnapshot, batch_size: usize) -> Result<(), StoreError> {
    for chunk in source.memories.chunks(batch_size) {
        for memory in chunk {
            insert_postgres_memory(tx, memory).await?;
        }
    }
    for (id, superseded_by) in &source.superseded_links {
        update_postgres_supersession(tx, id, superseded_by).await?;
    }
    for metadata in &source.metadata {
        insert_postgres_metadata(tx, metadata).await?;
    }
    import_postgres_contexts(tx, &source.contexts).await?;
    for audit in &source.audit_entries {
        insert_postgres_audit_entry(tx, audit).await?;
    }
    for tombstone in &source.tombstones {
        insert_postgres_tombstone(tx, tombstone).await?;
    }
    if let Some(profile) = &source.embedding_profile {
        insert_postgres_embedding_profile(tx, profile).await?;
    }
    reset_postgres_audit_sequence(tx).await?;
    reset_postgres_context_audit_sequence(tx).await?;
    Ok(())
}

async fn insert_postgres_memory(tx: &mut Transaction<'_, Postgres>, item: &MigrationMemory) -> Result<(), StoreError> {
    let memory = &item.memory;
    let impression_count = i64::try_from(memory.impression_count).map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let _result = query(
        "
        INSERT INTO memories (
            id, content, tags, provenance, access_policy, created_at, expires_at,
            has_embedding, embedding_revision, memory_type, importance, impression_count,
            last_impressed_at, superseded_by, activity_mass, last_used_at, updated_at, confidence, record_revision
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12,
            $13, NULL, $14, $15, $16, $17, $18
        )
        ",
    )
    .bind(memory.id.to_string())
    .bind(&memory.content)
    .bind(Json(memory.tags.clone()))
    .bind(Json(memory.provenance.clone()))
    .bind(Json(memory.access_policy.clone()))
    .bind(memory.created_at)
    .bind(memory.expires_at)
    .bind(memory.has_embedding)
    .bind(item.embedding_revision)
    .bind(memory.memory_type.to_string())
    .bind(memory.importance.value())
    .bind(impression_count)
    .bind(memory.last_impressed_at)
    .bind(memory.activity_mass)
    .bind(memory.last_used_at)
    .bind(memory.updated_at)
    .bind(memory.confidence.value())
    .bind(memory.record_revision)
    .execute(&mut **tx)
    .await?;

    if let Some(embedding) = &item.embedding {
        insert_postgres_embedding(tx, &memory.id, embedding).await?;
    }
    for entity in &memory.entities {
        let _result = query(
            "
            INSERT INTO memory_entities (memory_id, entity, entity_type)
            VALUES ($1, $2, $3)
            ",
        )
        .bind(memory.id.to_string())
        .bind(&entity.name)
        .bind(entity.entity_type.as_str())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_postgres_embedding(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId, embedding: &[f32]) -> Result<(), StoreError> {
    validate_embedding_vector(embedding, embedding.len())?;
    let vector = pgvector_literal(embedding);
    let _result = query(
        "
        INSERT INTO memory_embeddings (memory_id, embedding)
        VALUES ($1, $2::vector)
        ",
    )
    .bind(memory_id.to_string())
    .bind(vector)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn update_postgres_supersession(tx: &mut Transaction<'_, Postgres>, id: &MemoryId, superseded_by: &MemoryId) -> Result<(), StoreError> {
    let result = query("UPDATE memories SET superseded_by = $1 WHERE id = $2 AND superseded_by IS NULL")
        .bind(superseded_by.to_string())
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(StoreError::Conflict(format!("failed to restore supersession link from {id} to {superseded_by}")));
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "normalized context rows must be imported in foreign-key order")]
async fn import_postgres_contexts(tx: &mut Transaction<'_, Postgres>, source: &MigrationContextData) -> Result<(), StoreError> {
    for kind in &source.kinds {
        let _upserted = query(
            "INSERT INTO context_kinds (kind, display_name, builtin, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT(kind) DO UPDATE SET
                 display_name = EXCLUDED.display_name,
                 builtin = EXCLUDED.builtin,
                 enabled = EXCLUDED.enabled,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&kind.kind)
        .bind(&kind.display_name)
        .bind(kind.builtin)
        .bind(kind.enabled)
        .bind(kind.created_at)
        .bind(kind.updated_at)
        .execute(&mut **tx)
        .await?;
    }
    for context in &source.contexts {
        let definition = &context.definition;
        let _inserted = query(
            "INSERT INTO contexts (
                 id, kind, context_key, normalized_key, display_name, description,
                 owner_principal, guidance, parent_id, lifecycle, frozen, created_at, updated_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12)",
        )
        .bind(definition.id.to_string())
        .bind(definition.kind.as_str())
        .bind(&definition.key)
        .bind(&context.normalized_key)
        .bind(&definition.display_name)
        .bind(&definition.description)
        .bind(&definition.owner_principal)
        .bind(&definition.guidance)
        .bind(definition.lifecycle.to_string())
        .bind(definition.frozen)
        .bind(definition.created_at)
        .bind(definition.updated_at)
        .execute(&mut **tx)
        .await?;
    }
    for context in &source.contexts {
        let Some(parent_id) = context.definition.parent_id else {
            continue;
        };
        let _updated = query("UPDATE contexts SET parent_id = $1 WHERE id = $2")
            .bind(parent_id.to_string())
            .bind(context.definition.id.to_string())
            .execute(&mut **tx)
            .await?;
    }
    for alias in &source.aliases {
        let _inserted = query(
            "INSERT INTO context_aliases (context_id, alias, normalized_alias, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(alias.context_id.to_string())
        .bind(&alias.alias)
        .bind(&alias.normalized_alias)
        .bind(alias.created_at)
        .execute(&mut **tx)
        .await?;
    }
    for identity in &source.identities {
        let _inserted = query(
            "INSERT INTO context_identities (
                 context_id, owner_principal, kind, scheme, namespace,
                 fingerprint, redacted_label, created_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(identity.context_id.to_string())
        .bind(&identity.owner_principal)
        .bind(&identity.kind)
        .bind(&identity.identity.scheme)
        .bind(identity.identity.namespace.as_deref().unwrap_or_default())
        .bind(&identity.identity.fingerprint)
        .bind(&identity.identity.redacted_label)
        .bind(identity.created_at)
        .execute(&mut **tx)
        .await?;
    }
    for hint in &source.hints {
        let _inserted = query(
            "INSERT INTO context_resolver_hints (context_id, hint, normalized_hint, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(hint.context_id.to_string())
        .bind(&hint.hint)
        .bind(&hint.normalized_hint)
        .bind(hint.created_at)
        .execute(&mut **tx)
        .await?;
    }
    for grant in &source.grants {
        let _inserted = query(
            "INSERT INTO context_grants (context_id, grantee_principal, granted_by, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(grant.context_id.to_string())
        .bind(&grant.grantee_principal)
        .bind(&grant.granted_by)
        .bind(grant.created_at)
        .execute(&mut **tx)
        .await?;
    }
    for relation in &source.relations {
        let _inserted = query(
            "INSERT INTO context_relations (from_context_id, to_context_id, relation, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(relation.from_context_id.to_string())
        .bind(relation.to_context_id.to_string())
        .bind(&relation.relation)
        .bind(relation.created_at)
        .execute(&mut **tx)
        .await?;
    }
    for membership in &source.memberships {
        let _inserted = query(
            "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(membership.memory_id.to_string())
        .bind(membership.context_id.to_string())
        .bind(membership.ordinal)
        .bind(membership.created_at)
        .execute(&mut **tx)
        .await?;
    }
    for policy in &source.policies {
        let _inserted = query(
            "INSERT INTO context_kind_policies (layer, principal, kind, policy_json, updated_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&policy.layer)
        .bind(&policy.principal)
        .bind(&policy.kind)
        .bind(Json(policy.policy.clone()))
        .bind(policy.updated_at)
        .execute(&mut **tx)
        .await?;
    }
    for policy in &source.anchor_overrides {
        let _inserted = query(
            "INSERT INTO context_anchor_overrides (anchor_context_id, principal, policy_json, updated_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(policy.anchor_context_id.to_string())
        .bind(&policy.principal)
        .bind(Json(policy.policy.clone()))
        .bind(policy.updated_at)
        .execute(&mut **tx)
        .await?;
    }
    for event in &source.audit_events {
        let _inserted = query(
            "INSERT INTO context_audit_events (
                 id, actor_principal, action, context_id, memory_id, timestamp, details
             ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(event.id)
        .bind(&event.actor_principal)
        .bind(&event.action)
        .bind(event.context_id.map(|id| id.to_string()))
        .bind(event.memory_id.map(|id| id.to_string()))
        .bind(event.timestamp)
        .bind(event.details.clone().map(Json))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_postgres_metadata(tx: &mut Transaction<'_, Postgres>, metadata: &MigrationMetadata) -> Result<(), StoreError> {
    let row = &metadata.metadata;
    let _result = query(
        "
        INSERT INTO memory_metadata (
            memory_id, scope_key, summary, agent_label, created_by_principal,
            quality_flags, schema_version, migrated_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ",
    )
    .bind(row.memory_id.to_string())
    .bind(&row.scope_key)
    .bind(&row.summary)
    .bind(&row.agent_label)
    .bind(&row.created_by_principal)
    .bind(Json(row.quality_flags.clone()))
    .bind(row.schema_version)
    .bind(metadata.migrated_at)
    .bind(metadata.updated_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_postgres_audit_entry(tx: &mut Transaction<'_, Postgres>, audit: &MigrationAuditEntry) -> Result<(), StoreError> {
    let _result = query(
        "
        INSERT INTO memory_audit_log (id, memory_id, action, caller_agent, timestamp, details)
        VALUES ($1, $2, $3, $4, $5, $6)
        ",
    )
    .bind(audit.id)
    .bind(audit.memory_id.to_string())
    .bind(audit.entry.action.to_string())
    .bind(&audit.entry.caller_agent)
    .bind(audit.entry.timestamp)
    .bind(audit.entry.details.clone().map(Json))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_postgres_tombstone(tx: &mut Transaction<'_, Postgres>, tombstone: &MigrationTombstone) -> Result<(), StoreError> {
    let row = &tombstone.tombstone;
    let _result = query(
        "
        INSERT INTO memory_tombstones (memory_id, provenance, access_policy, deleted_at, deleted_by_principal)
        VALUES ($1, $2, $3, $4, $5)
        ",
    )
    .bind(row.memory_id.to_string())
    .bind(Json(row.provenance.clone()))
    .bind(Json(row.access_policy.clone()))
    .bind(row.deleted_at)
    .bind(&row.deleted_by_principal)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_postgres_embedding_profile(tx: &mut Transaction<'_, Postgres>, profile: &EmbeddingProfile) -> Result<(), StoreError> {
    let dimensions = i64::try_from(profile.dimensions).map_err(|error| StoreError::Serialization(Box::new(error)))?;
    let _result = query(
        "INSERT INTO embedding_profile (singleton, provider, endpoint, model, dimensions)
         VALUES (1, $1, $2, $3, $4)",
    )
    .bind(&profile.provider)
    .bind(&profile.endpoint)
    .bind(&profile.model)
    .bind(dimensions)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn reset_postgres_audit_sequence(tx: &mut Transaction<'_, Postgres>) -> Result<(), StoreError> {
    let _new_value: i64 = query_scalar(
        "
        SELECT setval(
            pg_get_serial_sequence('memory_audit_log', 'id'),
            COALESCE(MAX(id), 1),
            COUNT(*) > 0
        )
        FROM memory_audit_log
        ",
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(())
}

async fn reset_postgres_context_audit_sequence(tx: &mut Transaction<'_, Postgres>) -> Result<(), StoreError> {
    let _new_value: i64 = query_scalar(
        "
        SELECT setval(
            pg_get_serial_sequence('context_audit_events', 'id'),
            COALESCE(MAX(id), 1),
            COUNT(*) > 0
        )
        FROM context_audit_events
        ",
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(())
}

fn pgvector_literal(embedding: &[f32]) -> String {
    let mut vector = String::from("[");
    for (idx, value) in embedding.iter().enumerate() {
        if idx > 0 {
            vector.push(',');
        }
        vector.push_str(&value.to_string());
    }
    vector.push(']');
    vector
}

fn verify_counts(source: MigrationTableCounts, target: MigrationTableCounts) -> Result<(), StoreError> {
    if source != target {
        return Err(StoreError::Conflict(format!("migration verification failed: source {source:?}, target {target:?}")));
    }
    Ok(())
}

async fn verify_migrated_values(source: &MigrationSnapshot, tx: &mut Transaction<'_, Postgres>, embedding_dimensions: usize) -> Result<(), StoreError> {
    let target = export_postgres_tx(tx, embedding_dimensions).await?;
    let source_fingerprint = comparable_snapshot(source)?;
    let target_fingerprint = comparable_snapshot(&target)?;
    if source_fingerprint != target_fingerprint {
        return Err(StoreError::Conflict(
            "migration verification failed: target values differ from SQLite source snapshot".into(),
        ));
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct ComparableSnapshot {
    memories: Vec<ComparableMemory>,
    superseded_links: Vec<(String, String)>,
    audit_entries: Vec<ComparableAuditEntry>,
    tombstones: Vec<ComparableTombstone>,
    contexts: MigrationContextData,
    metadata: Vec<ComparableMetadata>,
    embedding_profile: Option<EmbeddingProfile>,
}

#[derive(serde::Serialize)]
struct ComparableMemory {
    memory: Memory,
    embedding_revision: i64,
    record_revision: i64,
    embedding: Option<Vec<f32>>,
}

#[derive(serde::Serialize)]
struct ComparableAuditEntry {
    id: i64,
    memory_id: MemoryId,
    action: AuditAction,
    caller_agent: Option<String>,
    timestamp_micros: i64,
    details: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct ComparableTombstone {
    tombstone: MemoryTombstone,
}

#[derive(serde::Serialize)]
struct ComparableMetadata {
    metadata: MemoryMetadata,
    migrated_at_micros: Option<i64>,
    updated_at_micros: i64,
}

fn comparable_snapshot(snapshot: &MigrationSnapshot) -> Result<serde_json::Value, StoreError> {
    let mut memories = snapshot.memories.iter().cloned().map(comparable_memory).collect::<Vec<_>>();
    memories.sort_by_key(|item| item.memory.id.to_string());

    let mut superseded_links = snapshot
        .superseded_links
        .iter()
        .map(|(id, superseded_by)| (id.to_string(), superseded_by.to_string()))
        .collect::<Vec<_>>();
    superseded_links.sort();

    let mut audit_entries = snapshot.audit_entries.iter().map(comparable_audit_entry).collect::<Vec<_>>();
    audit_entries.sort_by_key(|item| item.id);

    let mut tombstones = snapshot.tombstones.iter().cloned().map(comparable_tombstone).collect::<Vec<_>>();
    tombstones.sort_by_key(|item| item.tombstone.memory_id.to_string());

    let mut metadata = snapshot.metadata.iter().cloned().map(comparable_metadata).collect::<Vec<_>>();
    metadata.sort_by_key(|item| item.metadata.memory_id.to_string());

    let comparable = ComparableSnapshot {
        memories,
        superseded_links,
        audit_entries,
        tombstones,
        contexts: comparable_contexts(snapshot.contexts.clone()),
        metadata,
        embedding_profile: snapshot.embedding_profile.clone(),
    };
    serde_json::to_value(comparable).map_err(StoreError::from)
}

fn comparable_memory(mut item: MigrationMemory) -> ComparableMemory {
    item.memory.created_at = truncate_to_micros(item.memory.created_at);
    item.memory.updated_at = truncate_to_micros(item.memory.updated_at);
    item.memory.expires_at = item.memory.expires_at.map(truncate_to_micros);
    item.memory.last_impressed_at = item.memory.last_impressed_at.map(truncate_to_micros);
    item.memory.last_used_at = item.memory.last_used_at.map(truncate_to_micros);
    let record_revision = item.memory.record_revision;
    ComparableMemory {
        memory: item.memory,
        embedding_revision: item.embedding_revision,
        record_revision,
        embedding: item.embedding,
    }
}

fn comparable_audit_entry(item: &MigrationAuditEntry) -> ComparableAuditEntry {
    ComparableAuditEntry {
        id: item.id,
        memory_id: item.memory_id,
        action: item.entry.action,
        caller_agent: item.entry.caller_agent.clone(),
        timestamp_micros: timestamp_micros(item.entry.timestamp),
        details: item.entry.details.clone(),
    }
}

fn comparable_tombstone(mut item: MigrationTombstone) -> ComparableTombstone {
    item.tombstone.deleted_at = truncate_to_micros(item.tombstone.deleted_at);
    ComparableTombstone { tombstone: item.tombstone }
}

fn comparable_metadata(item: MigrationMetadata) -> ComparableMetadata {
    ComparableMetadata {
        metadata: item.metadata,
        migrated_at_micros: item.migrated_at.map(timestamp_micros),
        updated_at_micros: timestamp_micros(item.updated_at),
    }
}

fn comparable_contexts(mut contexts: MigrationContextData) -> MigrationContextData {
    for kind in &mut contexts.kinds {
        kind.created_at = truncate_to_micros(kind.created_at);
        kind.updated_at = truncate_to_micros(kind.updated_at);
    }
    for context in &mut contexts.contexts {
        context.definition.created_at = truncate_to_micros(context.definition.created_at);
        context.definition.updated_at = truncate_to_micros(context.definition.updated_at);
    }
    for alias in &mut contexts.aliases {
        alias.created_at = truncate_to_micros(alias.created_at);
    }
    for identity in &mut contexts.identities {
        identity.created_at = truncate_to_micros(identity.created_at);
    }
    for hint in &mut contexts.hints {
        hint.created_at = truncate_to_micros(hint.created_at);
    }
    for grant in &mut contexts.grants {
        grant.created_at = truncate_to_micros(grant.created_at);
    }
    for relation in &mut contexts.relations {
        relation.created_at = truncate_to_micros(relation.created_at);
    }
    for membership in &mut contexts.memberships {
        membership.created_at = truncate_to_micros(membership.created_at);
    }
    for policy in &mut contexts.policies {
        policy.updated_at = truncate_to_micros(policy.updated_at);
    }
    for policy in &mut contexts.anchor_overrides {
        policy.updated_at = truncate_to_micros(policy.updated_at);
    }
    for event in &mut contexts.audit_events {
        event.timestamp = truncate_to_micros(event.timestamp);
    }
    contexts
}

fn timestamp_micros(value: DateTime<Utc>) -> i64 {
    truncate_to_micros(value).timestamp_micros()
}

#[expect(clippy::arithmetic_side_effects, reason = "subtraction is bounded by timestamp_subsec_nanos modulo result")]
#[expect(clippy::integer_division_remainder_used, reason = "timestamp normalization intentionally drops sub-microsecond remainder")]
fn truncate_to_micros(value: DateTime<Utc>) -> DateTime<Utc> {
    let nanos = value.timestamp_subsec_nanos();
    let micros_nanos = nanos - (nanos % 1_000);
    DateTime::<Utc>::from_timestamp(value.timestamp(), micros_nanos).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, path::Path};

    use chrono::TimeZone as _;
    use serde_json::json;
    use sqlx_core::{query::query, query_scalar::query_scalar};

    use super::*;
    use crate::{
        store::{MemoryAdmin as _, MemoryReader as _, MemoryWriter as _},
        types::{AccessPolicy, Confidence, Entity, Importance, Memory, MemoryFilter, MemoryType, Provenance, QueryContext, ScopeDefinition},
    };

    const TEST_EMBEDDING_DIMENSIONS: usize = 3;
    type MissingRequiredCase = (&'static str, Vec<OsString>);
    type MetadataTimestamps = (Option<DateTime<Utc>>, DateTime<Utc>);

    struct MissingManagedKeyCase {
        table: &'static str,
        constraint: &'static str,
        expected_error: &'static str,
        cascade: bool,
    }

    #[derive(Debug)]
    struct SqliteFixture {
        old_id: MemoryId,
        new_id: MemoryId,
        deleted_id: MemoryId,
        old_embedding: Vec<f32>,
        new_embedding: Vec<f32>,
        old_embedding_revision: i64,
        new_embedding_revision: i64,
        entity: Entity,
        scope: ScopeDefinition,
        scope_updated_at: DateTime<Utc>,
        metadata: MemoryMetadata,
        metadata_migrated_at: DateTime<Utc>,
        metadata_updated_at: DateTime<Utc>,
        audit_details: serde_json::Value,
        tombstone: MemoryTombstone,
        counts: MigrationTableCounts,
    }

    #[test]
    fn sqlite_rejects_retired_iteration_metadata_table() {
        let connection = Connection::open_in_memory().unwrap();
        let _created = connection.execute("CREATE TABLE memory_v2_metadata (memory_id TEXT PRIMARY KEY)", []).unwrap();

        let error = reject_retired_sqlite_schema(&connection).unwrap_err();

        assert!(error.to_string().contains("unsupported prior-iteration table memory_v2_metadata"));
    }

    #[test]
    fn sqlite_fts_contract_accepts_equivalent_spacing_and_quoted_options() {
        validate_sqlite_fts_external_content(
            "CREATE VIRTUAL TABLE memory_fts USING\n fts5(content, content = 'memories', content_rowid = \"rowid\", tokenize='unicode61 remove_diacritics 2')",
        )
        .unwrap();
    }

    #[test]
    fn sqlite_fts_contract_ignores_comment_decoys() {
        let error = validate_sqlite_fts_external_content("CREATE VIRTUAL TABLE memory_fts USING fts5(content /*, content=memories, content_rowid=rowid, */)").unwrap_err();

        assert!(error.to_string().contains("external-content"));
    }

    #[test]
    fn sqlite_table_contract_ignores_comment_decoys() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE guarded (
                    frozen INTEGER /* CHECK (frozen IN (0, 1)) */
                )",
            )
            .unwrap();
        let expectation = SqliteTableExpectation::new("guarded", &["frozen"], &["check (frozen in (0, 1))"]);

        let error = validate_sqlite_table(&connection, &expectation).unwrap_err();

        assert!(error.to_string().contains("CHECK constraints"), "{error}");
    }

    #[test]
    fn sqlite_table_contract_ignores_quoted_literal_decoys_and_rejects_extra_checks() {
        let quoted = Connection::open_in_memory().unwrap();
        quoted
            .execute_batch(
                "CREATE TABLE guarded (
                    frozen INTEGER,
                    decoy TEXT DEFAULT 'CHECK (frozen IN (0, 1))'
                )",
            )
            .unwrap();
        let expectation = SqliteTableExpectation::new("guarded", &["frozen"], &["check (frozen in (0, 1))"]);
        let quoted_error = validate_sqlite_table(&quoted, &expectation).unwrap_err();
        assert!(quoted_error.to_string().contains("CHECK constraints"));

        let restrictive = Connection::open_in_memory().unwrap();
        restrictive
            .execute_batch(
                "CREATE TABLE guarded (
                    frozen INTEGER CHECK (frozen IN (0, 1)),
                    CHECK (frozen = 1)
                )",
            )
            .unwrap();
        let restrictive_error = validate_sqlite_table(&restrictive, &expectation).unwrap_err();
        assert!(restrictive_error.to_string().contains("CHECK constraints"));
    }

    #[test]
    fn sqlite_context_foreign_key_manifest_rejects_update_action_changes() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(crate::store::schema::CONTEXT_DDL).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 ALTER TABLE context_aliases RENAME TO context_aliases_old;
                 CREATE TABLE context_aliases (
                     context_id TEXT NOT NULL REFERENCES contexts(id) ON UPDATE CASCADE ON DELETE CASCADE,
                     alias TEXT NOT NULL,
                     normalized_alias TEXT NOT NULL,
                     created_at TEXT NOT NULL,
                     PRIMARY KEY (context_id, normalized_alias)
                 );
                 DROP TABLE context_aliases_old;",
            )
            .unwrap();

        let error = validate_sqlite_context_foreign_keys(&connection).unwrap_err();

        assert!(error.to_string().contains("foreign-key declarations"));
    }

    #[test]
    fn sqlite_context_integrity_handles_long_acyclic_parent_chain() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memories (id TEXT PRIMARY KEY);
                 CREATE TABLE memory_metadata (
                    memory_id TEXT PRIMARY KEY,
                    scope_key TEXT
                 );",
            )
            .unwrap();
        connection.execute_batch(crate::store::schema::CONTEXT_DDL).unwrap();
        let _inserted_kind = connection
            .execute(
                "INSERT INTO context_kinds (
                    kind, display_name, builtin, enabled, created_at, updated_at
                 ) VALUES ('custom', 'Custom', 1, 1, ?1, ?1)",
                ["2026-07-25T00:00:00Z"],
            )
            .unwrap();
        let mut parent_id = None::<String>;
        for index in 0_u32..1_500_u32 {
            let id = ContextId::new().to_string();
            let _inserted_context = connection
                .execute(
                    "INSERT INTO contexts (
                        id, kind, context_key, normalized_key, display_name,
                        owner_principal, parent_id, lifecycle, frozen,
                        created_at, updated_at
                     ) VALUES (?1, 'custom', ?2, ?2, ?2, 'owner', ?3,
                               'active', 0, ?4, ?4)",
                    rusqlite::params![id, format!("chain/{index}"), parent_id, "2026-07-25T00:00:00Z"],
                )
                .unwrap();
            parent_id = Some(id);
        }

        validate_sqlite_context_integrity(&connection).unwrap();
    }

    #[test]
    fn sqlite_fts_contract_accepts_comment_between_module_and_arguments() {
        validate_sqlite_fts_external_content("CREATE VIRTUAL TABLE memory_fts USING fts5/* gap */(content, content=memories, content_rowid=rowid)").unwrap();
    }

    #[test]
    fn sqlite_fts_contract_rejects_wrong_module_with_comment_decoy() {
        let error =
            validate_sqlite_fts_external_content("CREATE VIRTUAL TABLE memory_fts USING fts4(content /* using fts5(content=memories, content_rowid=rowid, */)").unwrap_err();

        assert!(error.to_string().contains("not an FTS5"));
    }

    #[test]
    fn sqlite_embedding_revision_contract_requires_not_null_default_zero() {
        let connection = Connection::open_in_memory().unwrap();
        let _created = connection.execute("CREATE TABLE memories (embedding_revision INTEGER)", []).unwrap();

        let error = validate_sqlite_embedding_revision_contract(&connection).unwrap_err();

        assert!(error.to_string().contains("embedding_revision"));
    }

    #[test]
    fn sqlite_embedding_revision_contract_accepts_startup_definition() {
        let connection = Connection::open_in_memory().unwrap();
        let _created = connection.execute("CREATE TABLE memories (embedding_revision INTEGER NOT NULL DEFAULT 0)", []).unwrap();

        validate_sqlite_embedding_revision_contract(&connection).unwrap();
    }

    #[test]
    fn parse_args_accepts_dry_run() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url"),
            OsString::from("postgres://localhost/db"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
        ];

        let parsed = SqliteToPostgresOptions::parse_args(&args).unwrap();

        assert_eq!(parsed.sqlite_path, PathBuf::from("source.db"));
        assert_eq!(parsed.postgres_url, "postgres://localhost/db");
        assert_eq!(parsed.embedding_dimensions, 768_usize);
        assert!(parsed.dry_run);
        assert!(!parsed.yes);
    }

    #[test]
    fn debug_redacts_migration_postgres_url_but_keeps_non_secret_options() {
        let options = SqliteToPostgresOptions {
            sqlite_path: PathBuf::from("source.db"),
            postgres_url: "postgres://private-user:private-password@db.example/localhold".into(),
            embedding_dimensions: 384,
            batch_size: 73,
            dry_run: true,
            yes: false,
        };

        let debug = format!("{options:?}");
        assert!(debug.contains("source.db"));
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("embedding_dimensions: 384"));
        assert!(debug.contains("batch_size: 73"));
        assert!(debug.contains("dry_run: true"));
        assert!(!debug.contains("private-user"));
        assert!(!debug.contains("private-password"));
        assert!(!debug.contains("db.example"));
    }

    #[test]
    fn invalid_pgvector_error_redacts_vector_text() {
        const VECTOR_TEXT: &str = "[0.125,not-a-number,derived-secret-sentinel]";
        let id = MemoryId::new();

        let error = invalid_pgvector_text_error(&id, VECTOR_TEXT);
        let message = error.to_string();

        assert!(message.contains(&id.to_string()));
        assert!(message.contains(&format!("{} bytes", VECTOR_TEXT.len())));
        assert!(!message.contains(VECTOR_TEXT));
        assert!(!message.contains("derived-secret-sentinel"));
    }

    #[test]
    fn parse_args_accepts_yes() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url"),
            OsString::from("postgres://localhost/db"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--yes"),
        ];

        let parsed = SqliteToPostgresOptions::parse_args(&args).unwrap();

        assert!(parsed.yes);
        assert!(!parsed.dry_run);
    }

    #[test]
    fn parse_args_accepts_default_postgres_url_env() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
        ];

        let parsed = SqliteToPostgresOptions::parse_args_with_env(&args, |name| {
            assert_eq!(name, DEFAULT_POSTGRES_URL_ENV);
            Ok("postgres://env/default".into())
        })
        .unwrap();

        assert_eq!(parsed.postgres_url, "postgres://env/default");
    }

    #[test]
    fn parse_args_accepts_custom_postgres_url_env() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url-env"),
            OsString::from("MIGRATION_DATABASE_URL"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
        ];

        let parsed = SqliteToPostgresOptions::parse_args_with_env(&args, |name| {
            assert_eq!(name, "MIGRATION_DATABASE_URL");
            Ok("postgres://env/custom".into())
        })
        .unwrap();

        assert_eq!(parsed.postgres_url, "postgres://env/custom");
    }

    #[test]
    fn parse_args_prefers_direct_postgres_url_over_env() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url"),
            OsString::from("postgres://argv/db"),
            OsString::from("--postgres-url-env"),
            OsString::from("MIGRATION_DATABASE_URL"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
        ];

        let env_was_read = Cell::new(false);
        let parsed = SqliteToPostgresOptions::parse_args_with_env(&args, |_| {
            env_was_read.set(true);
            Ok("postgres://env/db".into())
        })
        .unwrap();

        assert_eq!(parsed.postgres_url, "postgres://argv/db");
        assert!(!env_was_read.get(), "env should not be read when --postgres-url is set");
    }

    #[test]
    fn parse_args_rejects_missing_required_values() {
        let cases: [MissingRequiredCase; 3] = [
            ("--sqlite", vec![
                OsString::from("--postgres-url"),
                OsString::from("postgres://localhost/db"),
                OsString::from("--embedding-dimensions"),
                OsString::from("768"),
                OsString::from("--dry-run"),
            ]),
            (DEFAULT_POSTGRES_URL_ENV, vec![
                OsString::from("--sqlite"),
                OsString::from("source.db"),
                OsString::from("--embedding-dimensions"),
                OsString::from("768"),
                OsString::from("--dry-run"),
            ]),
            ("--embedding-dimensions", vec![
                OsString::from("--sqlite"),
                OsString::from("source.db"),
                OsString::from("--postgres-url"),
                OsString::from("postgres://localhost/db"),
                OsString::from("--dry-run"),
            ]),
        ];

        for (missing, args) in cases {
            let err = SqliteToPostgresOptions::parse_args_with_env(&args, |_| Err(VarError::NotPresent)).unwrap_err();
            assert!(err.to_string().contains(missing), "error should mention {missing}: {err}");
        }
    }

    #[test]
    fn parse_args_rejects_empty_postgres_url_env() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
        ];

        let err = SqliteToPostgresOptions::parse_args_with_env(&args, |_| Ok("  ".into())).unwrap_err();

        assert!(err.to_string().contains("environment variable LOCALHOLD_POSTGRES_URL is empty"));
    }

    #[test]
    fn parse_args_rejects_blank_postgres_url_env_name() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url-env"),
            OsString::from("  "),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
        ];

        let err = SqliteToPostgresOptions::parse_args_with_env(&args, |_| Ok("postgres://env/db".into())).unwrap_err();

        assert!(err.to_string().contains("--postgres-url-env requires a non-empty"));
    }

    #[test]
    fn parse_args_rejects_zero_batch_size() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url"),
            OsString::from("postgres://localhost/db"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--batch-size"),
            OsString::from("0"),
        ];

        let err = SqliteToPostgresOptions::parse_args(&args).unwrap_err();

        assert!(err.to_string().contains("batch-size"));
    }

    #[test]
    fn parse_args_rejects_dry_run_with_yes() {
        let args = [
            OsString::from("--sqlite"),
            OsString::from("source.db"),
            OsString::from("--postgres-url"),
            OsString::from("postgres://localhost/db"),
            OsString::from("--embedding-dimensions"),
            OsString::from("768"),
            OsString::from("--dry-run"),
            OsString::from("--yes"),
        ];

        let err = SqliteToPostgresOptions::parse_args(&args).unwrap_err();

        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn actual_migration_requires_yes_before_opening_source() {
        let options = SqliteToPostgresOptions {
            sqlite_path: PathBuf::from("does-not-exist.db"),
            postgres_url: "postgres://localhost/db".into(),
            embedding_dimensions: TEST_EMBEDDING_DIMENSIONS,
            batch_size: DEFAULT_BATCH_SIZE,
            dry_run: false,
            yes: false,
        };

        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("requires --yes"));
    }

    #[tokio::test]
    async fn sqlite_export_preserves_snapshot_data() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        let options = sqlite_options(&source_path, true, false);

        let snapshot = export_sqlite(&options).await.unwrap();

        assert_eq!(snapshot.counts, fixture.counts);
        assert_eq!(snapshot.superseded_links, vec![(fixture.old_id, fixture.new_id)]);
        let context = snapshot.contexts.contexts.iter().find(|context| context.definition.key == fixture.scope.scope_key).unwrap();
        assert_eq!(context.definition.display_name, fixture.scope.display_name);
        assert_eq!(context.definition.description, fixture.scope.description);
        assert_eq!(context.definition.updated_at, fixture.scope_updated_at);
        assert_eq!(snapshot.metadata.len(), 1_usize);
        assert_eq!(snapshot.metadata[0].metadata, fixture.metadata);
        assert_eq!(snapshot.metadata[0].migrated_at, Some(fixture.metadata_migrated_at));
        assert_eq!(snapshot.metadata[0].updated_at, fixture.metadata_updated_at);
        assert_eq!(snapshot.audit_entries.len(), 2_usize);
        assert_eq!(snapshot.audit_entries[0].memory_id, fixture.old_id);
        assert_eq!(snapshot.audit_entries[0].entry.action, AuditAction::Store);
        assert_eq!(snapshot.audit_entries[0].entry.details, Some(fixture.audit_details.clone()));
        assert_eq!(snapshot.audit_entries[1].memory_id, fixture.deleted_id);
        assert_eq!(snapshot.audit_entries[1].entry.action, AuditAction::Delete);
        assert_eq!(snapshot.tombstones.len(), 1_usize);
        assert_eq!(snapshot.tombstones[0].tombstone.memory_id, fixture.tombstone.memory_id);
        assert_eq!(snapshot.tombstones[0].tombstone.deleted_at, fixture.tombstone.deleted_at);
        assert_eq!(snapshot.tombstones[0].tombstone.deleted_by_principal, fixture.tombstone.deleted_by_principal);

        let old = snapshot.memories.iter().find(|memory| memory.memory.id == fixture.old_id).unwrap();
        assert_eq!(old.memory.superseded_by, None);
        assert_eq!(old.memory.entities, vec![fixture.entity.clone()]);
        assert_eq!(old.embedding.as_ref(), Some(&fixture.old_embedding));
        assert_eq!(old.embedding_revision, fixture.old_embedding_revision);

        let new = snapshot.memories.iter().find(|memory| memory.memory.id == fixture.new_id).unwrap();
        assert_eq!(new.embedding.as_ref(), Some(&fixture.new_embedding));
        assert_eq!(new.embedding_revision, fixture.new_embedding_revision);
    }

    #[tokio::test]
    async fn sqlite_export_allows_context_membership_without_metadata_cache() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        corrupt_sqlite_source(&source_path, move |conn| {
            let inserted = conn.execute(
                "INSERT INTO memory_contexts (memory_id, context_id, ordinal, created_at)
                 SELECT ?1, id, 0, ?2
                 FROM contexts
                 WHERE context_key = ?3",
                rusqlite::params![fixture.old_id.to_string(), fixed_time(9_u32).to_rfc3339(), fixture.scope.scope_key],
            )?;
            assert_eq!(inserted, 1_usize, "expected to insert one governed membership");
            let deleted = conn.execute("DELETE FROM memory_metadata WHERE memory_id = ?1", [fixture.old_id.to_string()])?;
            assert_eq!(deleted, 1_usize, "expected to delete one optional metadata cache row");
            Ok(())
        });

        let snapshot = export_sqlite(&sqlite_options(&source_path, true, false)).await.unwrap();

        assert!(snapshot.metadata.is_empty());
        assert_eq!(snapshot.contexts.memberships.len(), 1_usize);
    }

    #[tokio::test]
    async fn sqlite_export_allows_metadata_cache_without_context_membership() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;

        let snapshot = export_sqlite(&sqlite_options(&source_path, true, false)).await.unwrap();

        assert_eq!(snapshot.metadata.len(), 1_usize);
        assert!(snapshot.contexts.memberships.is_empty());
    }

    #[tokio::test]
    async fn sqlite_export_rejects_missing_embedding_for_embedded_memory() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        corrupt_sqlite_source(&source_path, move |conn| {
            let deleted = conn.execute("DELETE FROM memory_embedding_map WHERE memory_id = ?1", [fixture.old_id.to_string()])?;
            assert_eq!(deleted, 1_usize, "expected to delete one embedding map row");
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("has_embedding=true"));
    }

    #[tokio::test]
    async fn sqlite_export_rejects_stale_embedding_for_unembedded_memory() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        corrupt_sqlite_source(&source_path, move |conn| {
            let updated = conn.execute("UPDATE memories SET has_embedding = 0 WHERE id = ?1", [fixture.old_id.to_string()])?;
            assert_eq!(updated, 1_usize, "expected to update one memory embedding flag");
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("has_embedding=false"));
    }

    #[tokio::test]
    async fn sqlite_export_rejects_non_finite_embedding_blob() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        corrupt_sqlite_source(&source_path, move |conn| {
            let blob = sqlite_embedding_blob(&[f32::NAN, 0.2_f32, 0.3_f32]);
            let updated = conn.execute(
                "UPDATE memory_embeddings
                 SET embedding = ?1
                 WHERE rowid = (SELECT vec_rowid FROM memory_embedding_map WHERE memory_id = ?2)",
                rusqlite::params![blob, fixture.old_id.to_string()],
            )?;
            assert_eq!(updated, 1_usize, "expected to update one embedding blob");
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("non-finite"), "expected non-finite validation error, got: {err}");
    }

    #[tokio::test]
    async fn sqlite_export_rejects_source_dimension_mismatch_without_embeddings() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let store = SqliteStore::open(&source_path, TEST_EMBEDDING_DIMENSIONS + 1).unwrap();
        let memory = test_memory("unembedded mismatched source", 0_u32);
        let _id = store.store(&memory, None).await.unwrap();
        drop(store);
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("memory_embeddings table"), "expected dimension mismatch error, got: {err}");
    }

    #[tokio::test]
    async fn sqlite_export_rejects_source_dimension_mismatch_without_memories() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let store = SqliteStore::open(&source_path, TEST_EMBEDDING_DIMENSIONS + 1).unwrap();
        drop(store);
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("memory_embeddings table"), "expected dimension mismatch error, got: {err}");
    }

    #[tokio::test]
    async fn sqlite_export_rejects_missing_source_schema_table() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        corrupt_sqlite_source(&source_path, |conn| {
            conn.execute_batch("DROP TABLE memory_metadata")?;
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("memory_metadata"), "expected missing table error, got: {err}");
        assert!(err.to_string().contains("open the source database once"), "expected repair guidance, got: {err}");
    }

    #[tokio::test]
    async fn sqlite_export_rejects_missing_source_schema_trigger() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        corrupt_sqlite_source(&source_path, |conn| {
            conn.execute_batch("DROP TRIGGER trg_memory_fts_insert")?;
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("trg_memory_fts_insert"), "expected missing trigger error, got: {err}");
        assert!(err.to_string().contains("open the source database once"), "expected repair guidance, got: {err}");

        let repaired = SqliteStore::open(&source_path, TEST_EMBEDDING_DIMENSIONS).unwrap();
        drop(repaired);
        let snapshot = export_sqlite(&options).await.unwrap();
        assert_eq!(snapshot.counts, fixture.counts);
    }

    #[tokio::test]
    async fn sqlite_export_rejects_embedding_map_with_missing_vector_row() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let store = SqliteStore::open(&source_path, TEST_EMBEDDING_DIMENSIONS).unwrap();
        let memory = test_memory("unembedded stale mapping", 0_u32);
        let id = store.store(&memory, None).await.unwrap();
        drop(store);
        corrupt_sqlite_source(&source_path, move |conn| {
            let inserted = conn.execute("INSERT INTO memory_embedding_map (memory_id, vec_rowid) VALUES (?1, ?2)", rusqlite::params![
                id.to_string(),
                99_999_i64
            ])?;
            assert_eq!(inserted, 1_usize, "expected to insert one dangling embedding map row");
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let err = export_sqlite(&options).await.unwrap_err();

        assert!(err.to_string().contains("vec_rowid"), "expected dangling vector row error, got: {err}");
    }

    #[tokio::test]
    async fn migration_source_preflight_runs_before_target_for_dry_run_and_actual() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let store = SqliteStore::open(&source_path, TEST_EMBEDDING_DIMENSIONS + 1).unwrap();
        drop(store);

        for (dry_run, yes) in [(true, false), (false, true)] {
            let mut options = sqlite_options(&source_path, dry_run, yes);
            options.postgres_url = "postgres://target-preflight-should-not-open/localhold".into();

            let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

            assert!(err.to_string().contains("memory_embeddings table"), "expected source preflight error, got: {err}");
            assert!(!err.to_string().contains("database error"), "target should not be opened before source preflight: {err}");
        }
    }

    #[tokio::test]
    async fn migration_source_preflight_rejects_foreign_key_violations_before_target_for_dry_run_and_actual() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        let missing_id = MemoryId::new();
        corrupt_sqlite_source(&source_path, move |conn| {
            conn.pragma_update(None, "foreign_keys", false)?;
            let inserted = conn.execute("INSERT INTO memory_entities (memory_id, entity, entity_type) VALUES (?1, ?2, ?3)", rusqlite::params![
                missing_id.to_string(),
                "Orphan Entity",
                "project"
            ])?;
            assert_eq!(inserted, 1_usize, "expected to insert one orphan entity row");
            let inserted = conn.execute(
                "INSERT INTO memory_metadata (memory_id, quality_flags, schema_version, updated_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![missing_id.to_string(), "[]", 2_i64, fixed_time(10_u32).to_rfc3339()],
            )?;
            assert_eq!(inserted, 1_usize, "expected to insert one orphan metadata row");
            Ok(())
        });

        for (dry_run, yes) in [(true, false), (false, true)] {
            let mut options = sqlite_options(&source_path, dry_run, yes);
            options.postgres_url = "postgres://target-preflight-should-not-open/localhold".into();

            let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

            assert!(err.to_string().contains("foreign key violation"), "expected source foreign-key error, got: {err}");
            assert!(!err.to_string().contains("open the source database once"), "FK violations need data repair guidance: {err}");
            assert!(!err.to_string().contains("database error"), "target should not be opened before source preflight: {err}");
        }
    }

    #[tokio::test]
    async fn sqlite_export_rejects_missing_supersession_target() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        let missing_id = MemoryId::new();
        corrupt_sqlite_source(&source_path, move |conn| {
            let updated = conn.execute("UPDATE memories SET superseded_by = ?1 WHERE id = ?2", [missing_id.to_string(), fixture.old_id.to_string()])?;
            assert_eq!(updated, 1_usize, "expected to update one supersession link");
            Ok(())
        });
        let options = sqlite_options(&source_path, true, false);

        let snapshot = export_sqlite(&options).await.unwrap();
        let err = validate_supersession_links(&snapshot).unwrap_err();

        assert!(err.to_string().contains("superseded by missing memory"));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_dry_run_does_not_write_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        drop_postgres_migration_schema().await;
        let options = sqlite_options(&source_path, true, false);

        let summary = migrate_sqlite_to_postgres(&options).await.unwrap();

        assert_eq!(summary.source, fixture.counts);
        assert_eq!(summary.target_before, MigrationTableCounts::default());
        assert_eq!(summary.target_after, None);
        assert!(summary.dry_run);

        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let memories_table_exists: bool = query_scalar("SELECT to_regclass('memories') IS NOT NULL").fetch_one(&pool).await.unwrap();
        assert!(!memories_table_exists);
        let target_counts = postgres_counts_existing(&pool).await.unwrap();
        assert_eq!(target_counts, MigrationTableCounts::default());
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_migrates_core_data_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        reset_postgres_migration_database().await;
        let options = sqlite_options(&source_path, false, true);

        let summary = migrate_sqlite_to_postgres(&options).await.unwrap();

        assert_eq!(summary.source, fixture.counts);
        assert_eq!(summary.target_before, MigrationTableCounts {
            context_kinds: 4,
            ..MigrationTableCounts::default()
        });
        assert_eq!(summary.target_after, Some(fixture.counts));
        assert!(!summary.dry_run);

        let target = open_postgres_migration_store().await;
        let old = target.get(&fixture.old_id, Some("migration-agent")).await.unwrap().unwrap();
        assert_eq!(old.superseded_by, Some(fixture.new_id));
        assert_eq!(old.entities, vec![fixture.entity.clone()]);

        let metadata = target.get_metadata(&fixture.old_id).await.unwrap();
        assert_eq!(metadata, Some(fixture.metadata.clone()));

        let audit_entries = target.query_audit_log(&fixture.old_id, 10_usize).await.unwrap();
        assert_eq!(audit_entries.len(), 1_usize);
        assert_eq!(audit_entries[0].details, Some(fixture.audit_details.clone()));
        let deleted_audit_entries = target.query_audit_log(&fixture.deleted_id, 10_usize).await.unwrap();
        assert_eq!(deleted_audit_entries.len(), 1_usize);
        assert_eq!(deleted_audit_entries[0].action, AuditAction::Delete);
        let tombstone = target.get_tombstone(&fixture.deleted_id).await.unwrap().unwrap();
        assert_eq!(tombstone.memory_id, fixture.tombstone.memory_id);
        assert_eq!(tombstone.provenance.source_agent, fixture.tombstone.provenance.source_agent);
        assert_eq!(tombstone.deleted_by_principal, fixture.tombstone.deleted_by_principal);

        let embeddings = target.fetch_embeddings_for_ids(&[fixture.old_id, fixture.new_id]).await.unwrap();
        assert_eq!(embeddings.get(&fixture.old_id), Some(&fixture.old_embedding));
        assert_eq!(embeddings.get(&fixture.new_id), Some(&fixture.new_embedding));

        let old_revision: i64 = query_scalar("SELECT embedding_revision FROM memories WHERE id = $1")
            .bind(fixture.old_id.to_string())
            .fetch_one(target.pool())
            .await
            .unwrap();
        assert_eq!(old_revision, fixture.old_embedding_revision);
        let new_revision: i64 = query_scalar("SELECT embedding_revision FROM memories WHERE id = $1")
            .bind(fixture.new_id.to_string())
            .fetch_one(target.pool())
            .await
            .unwrap();
        assert_eq!(new_revision, fixture.new_embedding_revision);

        let scope_updated_at: DateTime<Utc> = query_scalar("SELECT updated_at FROM contexts WHERE context_key = $1")
            .bind(&fixture.scope.scope_key)
            .fetch_one(target.pool())
            .await
            .unwrap();
        assert_eq!(scope_updated_at, fixture.scope_updated_at);
        let metadata_timestamps: MetadataTimestamps = sqlx_core::query_as::query_as("SELECT migrated_at, updated_at FROM memory_metadata WHERE memory_id = $1")
            .bind(fixture.old_id.to_string())
            .fetch_one(target.pool())
            .await
            .unwrap();
        assert_eq!(metadata_timestamps.0, Some(fixture.metadata_migrated_at));
        assert_eq!(metadata_timestamps.1, fixture.metadata_updated_at);

        let results = target
            .search_by_embedding(
                &fixture.new_embedding,
                10_usize,
                &MemoryFilter::default(),
                &QueryContext {
                    principal: Some("migration-agent".into()),
                },
                Some(0.001_f64),
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0].memory.id, fixture.new_id);
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_migrates_empty_target_missing_claim_columns_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _dropped_index = query("DROP INDEX IF EXISTS idx_memories_embedding_claim").execute(&pool).await.unwrap();
        let _dropped_columns = query("ALTER TABLE memories DROP COLUMN embedding_claimed_at, DROP COLUMN embedding_claim_token")
            .execute(&pool)
            .await
            .unwrap();
        let options = sqlite_options(&source_path, false, true);

        let summary = migrate_sqlite_to_postgres(&options).await.unwrap();

        assert_eq!(summary.source, fixture.counts);
        assert_eq!(summary.target_before, MigrationTableCounts {
            context_kinds: 4,
            ..MigrationTableCounts::default()
        });
        assert_eq!(summary.target_after, Some(fixture.counts));
        let claim_column_count: i64 = query_scalar(
            "
            SELECT COUNT(*)
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'memories'
              AND column_name IN ('embedding_claimed_at', 'embedding_claim_token')
            ",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(claim_column_count, 2_i64);
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_migrates_empty_target_partially_missing_claim_columns_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let fixture = seed_sqlite_source(&source_path).await;
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _dropped_column = query("ALTER TABLE memories DROP COLUMN embedding_claim_token").execute(&pool).await.unwrap();
        let options = sqlite_options(&source_path, false, true);

        let summary = migrate_sqlite_to_postgres(&options).await.unwrap();

        assert_eq!(summary.source, fixture.counts);
        assert_eq!(summary.target_before, MigrationTableCounts {
            context_kinds: 4,
            ..MigrationTableCounts::default()
        });
        assert_eq!(summary.target_after, Some(fixture.counts));
        let claim_column_count: i64 = query_scalar(
            "
            SELECT COUNT(*)
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'memories'
              AND column_name IN ('embedding_claimed_at', 'embedding_claim_token')
            ",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(claim_column_count, 2_i64);
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_non_empty_target_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        reset_postgres_migration_database().await;
        let target = open_postgres_migration_store().await;
        let existing = test_memory("already present", 5_u32);
        let _existing_id = target.store(&existing, None).await.unwrap();

        let options = sqlite_options(&source_path, false, true);
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("PostgreSQL target is not empty"));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_partial_non_empty_target_before_bootstrap_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        drop_postgres_migration_schema().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _created = query("CREATE TABLE memories (id TEXT PRIMARY KEY)").execute(&pool).await.unwrap();
        let _inserted = query("INSERT INTO memories (id) VALUES ($1)")
            .bind(MemoryId::new().to_string())
            .execute(&pool)
            .await
            .unwrap();

        let options = sqlite_options(&source_path, false, true);
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("partial managed schema"));
        let migrations_table_exists: bool = query_scalar("SELECT to_regclass('localhold_migrations') IS NOT NULL").fetch_one(&pool).await.unwrap();
        assert!(!migrations_table_exists, "non-empty target should be refused before schema bootstrap");
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_malformed_empty_target_before_bootstrap_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        drop_postgres_migration_schema().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _created = query("CREATE TABLE memories (id TEXT PRIMARY KEY)").execute(&pool).await.unwrap();

        let options = sqlite_options(&source_path, false, true);
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("partial managed schema"));
        let migrations_table_exists: bool = query_scalar("SELECT to_regclass('localhold_migrations') IS NOT NULL").fetch_one(&pool).await.unwrap();
        assert!(!migrations_table_exists, "malformed target should be refused before schema bootstrap");
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_malformed_migrations_table_before_bootstrap_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        drop_postgres_migration_schema().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _created = query("CREATE TABLE localhold_migrations (version TEXT PRIMARY KEY)").execute(&pool).await.unwrap();

        let options = sqlite_options(&source_path, false, true);
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("localhold_migrations.version"));
        let memories_table_exists: bool = query_scalar("SELECT to_regclass('memories') IS NOT NULL").fetch_one(&pool).await.unwrap();
        assert!(!memories_table_exists, "malformed migrations table should be refused before user table bootstrap");
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_migrations_table_without_version_constraint_before_bootstrap_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        drop_postgres_migration_schema().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _created = query(
            "
            CREATE TABLE localhold_migrations (
                version BIGINT,
                name TEXT,
                applied_at TIMESTAMPTZ
            )
            ",
        )
        .execute(&pool)
        .await
        .unwrap();

        let options = sqlite_options(&source_path, false, true);
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("localhold_migrations.version"));
        let memories_table_exists: bool = query_scalar("SELECT to_regclass('memories') IS NOT NULL").fetch_one(&pool).await.unwrap();
        assert!(!memories_table_exists, "malformed migrations table should be refused before user table bootstrap");
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_existing_schema_with_wrong_column_type_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _altered = query("ALTER TABLE contexts ALTER COLUMN description TYPE VARCHAR(100)").execute(&pool).await.unwrap();

        let options = sqlite_options(&source_path, false, true);
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(err.to_string().contains("contexts.description"));
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_missing_non_migrated_confidence_column() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _dropped_table = query("DROP TABLE memory_audit_log").execute(&pool).await.unwrap();
        let _dropped_column = query("ALTER TABLE memories DROP COLUMN confidence").execute(&pool).await.unwrap();

        let err = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(err.to_string().contains("table memories is missing required column confidence"), "{err}");
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_retired_iteration_metadata_table() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _created = query("CREATE TABLE memory_v2_metadata (memory_id TEXT PRIMARY KEY)").execute(&pool).await.unwrap();

        let error = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(error.to_string().contains("unsupported prior-iteration table memory_v2_metadata"));
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_relaxed_required_nullability() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _altered = query("ALTER TABLE memories ALTER COLUMN content DROP NOT NULL").execute(&pool).await.unwrap();

        let err = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(err.to_string().contains("memories.content"), "{err}");
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_accepts_long_acyclic_context_hierarchy() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _inserted = query(
            "
            INSERT INTO contexts (
                id, kind, context_key, normalized_key, display_name,
                owner_principal, parent_id, created_at, updated_at
            )
            SELECT
                'long-chain-' || node,
                'custom',
                'custom/long-chain-' || node,
                'custom/long-chain-' || node,
                'Long chain ' || node,
                'chain-owner',
                CASE WHEN node = 1 THEN NULL ELSE 'long-chain-' || (node - 1) END,
                NOW(),
                NOW()
            FROM generate_series(1, 1500) AS node
            ORDER BY node
            ",
        )
        .execute(&pool)
        .await
        .unwrap();

        validate_ready_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap();

        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_context_parent_cycle() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _inserted = query(
            "
            INSERT INTO contexts (
                id, kind, context_key, normalized_key, display_name,
                owner_principal, created_at, updated_at
            ) VALUES
                ('cycle-a', 'custom', 'custom/cycle-a', 'custom/cycle-a', 'Cycle A', 'cycle-owner', NOW(), NOW()),
                ('cycle-b', 'custom', 'custom/cycle-b', 'custom/cycle-b', 'Cycle B', 'cycle-owner', NOW(), NOW())
            ",
        )
        .execute(&pool)
        .await
        .unwrap();
        let _cycled = query(
            "
            UPDATE contexts
            SET parent_id = CASE id
                WHEN 'cycle-a' THEN 'cycle-b'
                WHEN 'cycle-b' THEN 'cycle-a'
            END
            WHERE id IN ('cycle-a', 'cycle-b')
            ",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = validate_ready_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(error.to_string().contains("parent cycle"), "{error}");
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_non_sequence_audit_id_default() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _altered = query("ALTER TABLE memory_audit_log ALTER COLUMN id SET DEFAULT 1").execute(&pool).await.unwrap();

        let err = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(err.to_string().contains("memory_audit_log.id"), "{err}");
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_non_sequence_context_audit_id_default() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _altered = query("ALTER TABLE context_audit_events ALTER COLUMN id SET DEFAULT 1").execute(&pool).await.unwrap();

        let err = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(err.to_string().contains("context_audit_events.id"), "{err}");
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_cycling_audit_id_sequence() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _altered = query("ALTER SEQUENCE memory_audit_log_id_seq MAXVALUE 2 CYCLE").execute(&pool).await.unwrap();

        let err = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(err.to_string().contains("memory_audit_log.id"));
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn present_postgres_schema_rejects_audit_sequence_behind_existing_ids() {
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _inserted = query("INSERT INTO memory_audit_log (id, memory_id, action, timestamp) VALUES (1, 'seeded', 'remember', NOW())")
            .execute(&pool)
            .await
            .unwrap();
        let _reset = query("SELECT setval('memory_audit_log_id_seq', 1, false)").execute(&pool).await.unwrap();

        let err = validate_present_postgres_schema(&pool, TEST_EMBEDDING_DIMENSIONS, true, true).await.unwrap_err();

        assert!(err.to_string().contains("unused next value"));
        pool.close().await;
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_rejects_existing_schema_missing_managed_key_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        let cases = [
            MissingManagedKeyCase {
                table: "memories",
                constraint: "memories_pkey",
                expected_error: "memories.id",
                cascade: true,
            },
            MissingManagedKeyCase {
                table: "memory_entities",
                constraint: "memory_entities_pkey",
                expected_error: "memory_entities(memory_id, entity, entity_type)",
                cascade: false,
            },
            MissingManagedKeyCase {
                table: "memory_embeddings",
                constraint: "memory_embeddings_pkey",
                expected_error: "memory_embeddings.memory_id",
                cascade: false,
            },
            MissingManagedKeyCase {
                table: "memory_audit_log",
                constraint: "memory_audit_log_pkey",
                expected_error: "memory_audit_log.id",
                cascade: false,
            },
            MissingManagedKeyCase {
                table: "memory_tombstones",
                constraint: "memory_tombstones_pkey",
                expected_error: "memory_tombstones.memory_id",
                cascade: false,
            },
            MissingManagedKeyCase {
                table: "contexts",
                constraint: "contexts_owner_principal_kind_normalized_key_key",
                expected_error: "contexts(owner_principal, kind, normalized_key)",
                cascade: false,
            },
            MissingManagedKeyCase {
                table: "memory_metadata",
                constraint: "memory_metadata_pkey",
                expected_error: "memory_metadata.memory_id",
                cascade: false,
            },
        ];

        for case in cases {
            drop_postgres_migration_schema().await;
            reset_postgres_migration_database().await;
            drop_postgres_constraint(&case).await;

            let options = sqlite_options(&source_path, false, true);
            let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

            assert!(err.to_string().contains(case.expected_error), "expected error to mention {}: {err}", case.expected_error);
        }
        drop_postgres_migration_schema().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
    async fn sqlite_to_postgres_times_out_waiting_for_target_locks_against_postgres() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source.db");
        let _fixture = seed_sqlite_source(&source_path).await;
        reset_postgres_migration_database().await;
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let mut blocker = pool.begin().await.unwrap();
        let _locked = query("LOCK TABLE memories IN ACCESS SHARE MODE").execute(&mut *blocker).await.unwrap();

        let options = sqlite_options(&source_path, false, true);
        let started = std::time::Instant::now();
        let err = migrate_sqlite_to_postgres(&options).await.unwrap_err();

        assert!(started.elapsed() < std::time::Duration::from_secs(10), "schema migration timeout was not bounded");
        assert!(err.to_string().contains("timed out waiting for PostgreSQL schema migration locks"), "{err}");
        blocker.rollback().await.unwrap();
        let report = migrate_sqlite_to_postgres(&options).await.unwrap();
        assert_eq!(report.target_after.unwrap().memories, 2_u64);
        drop_postgres_migration_schema().await;
    }

    #[test]
    fn counts_empty_only_when_all_user_tables_empty() {
        assert!(MigrationTableCounts::default().is_empty());
        let cases = [
            MigrationTableCounts {
                memories: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                entities: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                embeddings: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                audit_entries: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                tombstones: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_kinds: 5,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                contexts: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_aliases: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_identities: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_hints: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_grants: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_relations: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                memory_contexts: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_policies: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_anchor_overrides: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                context_audit_events: 1,
                ..MigrationTableCounts::default()
            },
            MigrationTableCounts {
                metadata: 1,
                ..MigrationTableCounts::default()
            },
        ];
        for counts in cases {
            assert!(!counts.is_empty(), "{counts:?} should not be empty");
        }
    }

    #[test]
    fn comparable_snapshot_sorts_memories_after_timestamp_normalization() {
        let base = fixed_time(0_u32);
        let mut first = test_memory("first same microsecond", 0_u32);
        first.created_at = base + chrono::TimeDelta::nanoseconds(900_i64);
        first.updated_at = first.created_at;
        let mut second = test_memory("second same microsecond", 0_u32);
        second.created_at = base + chrono::TimeDelta::nanoseconds(100_i64);
        second.updated_at = second.created_at;

        let first = MigrationMemory {
            memory: first,
            embedding_revision: 0_i64,
            embedding: None,
        };
        let second = MigrationMemory {
            memory: second,
            embedding_revision: 0_i64,
            embedding: None,
        };
        let source = migration_snapshot_with_memories(vec![first.clone(), second.clone()]);
        let target = migration_snapshot_with_memories(vec![second, first]);

        assert_eq!(comparable_snapshot(&source).unwrap(), comparable_snapshot(&target).unwrap());
    }

    #[test]
    fn comparable_snapshot_includes_record_revision() {
        let source_memory = test_memory("revision source", 0_u32);
        let mut target_memory = source_memory.clone();
        target_memory.record_revision = source_memory.record_revision + 1_i64;
        let source = migration_snapshot_with_memories(vec![MigrationMemory {
            memory: source_memory,
            embedding_revision: 0_i64,
            embedding: None,
        }]);
        let target = migration_snapshot_with_memories(vec![MigrationMemory {
            memory: target_memory,
            embedding_revision: 0_i64,
            embedding: None,
        }]);

        assert_ne!(comparable_snapshot(&source).unwrap(), comparable_snapshot(&target).unwrap());
    }

    fn migration_snapshot_with_memories(memories: Vec<MigrationMemory>) -> MigrationSnapshot {
        MigrationSnapshot {
            memories,
            superseded_links: Vec::new(),
            audit_entries: Vec::new(),
            tombstones: Vec::new(),
            contexts: MigrationContextData::default(),
            metadata: Vec::new(),
            embedding_profile: None,
            counts: MigrationTableCounts::default(),
        }
    }

    #[expect(clippy::too_many_lines, reason = "fixture setup is linear to keep migration field coverage explicit")]
    async fn seed_sqlite_source(path: &Path) -> SqliteFixture {
        let store = SqliteStore::open(path, TEST_EMBEDDING_DIMENSIONS).unwrap();
        store
            .verify_embedding_profile(&EmbeddingProfile::openai_compatible(
                "http://127.0.0.1:8000/v1",
                "migration-test-model",
                TEST_EMBEDDING_DIMENSIONS,
            ))
            .await
            .unwrap();
        let entity = Entity::new("Migration Entity", "project").unwrap();
        let old_embedding_revision = 7_i64;
        let new_embedding_revision = 11_i64;
        let scope_updated_at = fixed_time(6_u32);
        let metadata_migrated_at = fixed_time(7_u32);
        let metadata_updated_at = fixed_time(8_u32);

        let mut old = test_memory("old migrated memory", 0_u32);
        old.tags = vec!["migration".into(), "old".into()];
        old.entities = vec![entity.clone()];
        let old_embedding = vec![0.1_f32, 0.2_f32, 0.3_f32];
        let old_id = store.store(&old, Some(&old_embedding)).await.unwrap();

        let mut new = test_memory("new migrated memory", 1_u32);
        new.tags = vec!["migration".into(), "new".into()];
        let new_embedding = vec![0.3_f32, 0.2_f32, 0.1_f32];
        let new_id = store.store_with_supersession(&new, Some(&new_embedding), &old_id).await.unwrap();

        let scope = ScopeDefinition {
            scope_key: "migration/scope".into(),
            display_name: "Migration Scope".into(),
            description: Some("scope copied by migration".into()),
            aliases: vec!["migration-alias".into()],
            matchers: vec!["migration/source".into()],
            parent: Some("migration".into()),
            related: vec!["migration/related".into()],
        };
        store.register_scope(scope.clone()).await.unwrap();

        let metadata = MemoryMetadata {
            memory_id: old_id,
            scope_key: Some(scope.scope_key.clone()),
            summary: Some("copied summary".into()),
            agent_label: Some("migration-agent".into()),
            created_by_principal: Some("migration-principal".into()),
            quality_flags: vec!["seeded".into()],
            schema_version: 1_i64,
        };
        store.upsert_metadata(metadata.clone()).await.unwrap();

        let old_id_for_update = old_id.to_string();
        let new_id_for_update = new_id.to_string();
        let scope_key_for_update = scope.scope_key.clone();
        let metadata_id_for_update = old_id.to_string();
        store
            .with_conn(move |conn| {
                let updated = conn.execute("UPDATE memories SET embedding_revision = ?1 WHERE id = ?2", rusqlite::params![
                    old_embedding_revision,
                    old_id_for_update
                ])?;
                assert_eq!(updated, 1_usize, "expected to update old memory embedding revision");
                let updated = conn.execute("UPDATE memories SET embedding_revision = ?1 WHERE id = ?2", rusqlite::params![
                    new_embedding_revision,
                    new_id_for_update
                ])?;
                assert_eq!(updated, 1_usize, "expected to update new memory embedding revision");
                let updated = conn.execute("UPDATE contexts SET updated_at = ?1 WHERE context_key = ?2", rusqlite::params![
                    scope_updated_at.to_rfc3339(),
                    scope_key_for_update
                ])?;
                assert_eq!(updated, 1_usize, "expected to update scope timestamp");
                let updated = conn.execute("UPDATE memory_metadata SET migrated_at = ?1, updated_at = ?2 WHERE memory_id = ?3", rusqlite::params![
                    metadata_migrated_at.to_rfc3339(),
                    metadata_updated_at.to_rfc3339(),
                    metadata_id_for_update
                ])?;
                assert_eq!(updated, 1_usize, "expected to update metadata timestamps");
                Ok(())
            })
            .await
            .unwrap();

        let audit_details = json!({ "phase": "seed" });
        store
            .write_audit_entry(&old_id, AuditAction::Store, Some("migration-agent"), fixed_time(2_u32), Some(&audit_details))
            .await
            .unwrap();
        let deleted_id = MemoryId::new();
        let tombstone = MemoryTombstone {
            memory_id: deleted_id,
            provenance: Provenance {
                source_agent: Some("migration-agent".into()),
                source_conversation: Some("migration/deleted".into()),
                origin_conversation: Some("migration/deleted-origin".into()),
                source_user: None,
            },
            access_policy: AccessPolicy::Restricted {
                allowed: vec!["migration-friend".into()],
            },
            deleted_at: fixed_time(9_u32),
            deleted_by_principal: Some("migration-agent".into()),
        };
        store
            .write_audit_entry(&deleted_id, AuditAction::Delete, Some("migration-agent"), fixed_time(9_u32), None)
            .await
            .unwrap();
        let tombstone_for_insert = tombstone.clone();
        store
            .with_conn(move |conn| {
                let provenance = serde_json::to_string(&tombstone_for_insert.provenance).unwrap();
                let access_policy = serde_json::to_string(&tombstone_for_insert.access_policy).unwrap();
                let inserted = conn.execute(
                    "INSERT INTO memory_tombstones (memory_id, provenance, access_policy, deleted_at, deleted_by_principal) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        tombstone_for_insert.memory_id.to_string(),
                        provenance,
                        access_policy,
                        tombstone_for_insert.deleted_at.to_rfc3339(),
                        tombstone_for_insert.deleted_by_principal,
                    ],
                )?;
                assert_eq!(inserted, 1_usize, "expected to insert one tombstone");
                Ok(())
            })
            .await
            .unwrap();
        drop(store);

        SqliteFixture {
            old_id,
            new_id,
            deleted_id,
            old_embedding,
            new_embedding,
            old_embedding_revision,
            new_embedding_revision,
            entity,
            scope,
            scope_updated_at,
            metadata,
            metadata_migrated_at,
            metadata_updated_at,
            audit_details,
            tombstone,
            counts: MigrationTableCounts {
                memories: 2_u64,
                entities: 1_u64,
                embeddings: 2_u64,
                audit_entries: 2_u64,
                tombstones: 1_u64,
                context_kinds: 4_u64,
                contexts: 3_u64,
                context_aliases: 1_u64,
                context_hints: 1_u64,
                context_grants: 3_u64,
                context_relations: 1_u64,
                context_audit_events: 1_u64,
                metadata: 1_u64,
                embedding_profiles: 1_u64,
                ..MigrationTableCounts::default()
            },
        }
    }

    fn test_memory(content: &str, minute: u32) -> Memory {
        let now = fixed_time(minute);
        Memory {
            id: MemoryId::new(),
            content: content.into(),
            tags: vec!["migration".into()],
            provenance: Provenance {
                source_agent: Some("migration-agent".into()),
                source_conversation: Some("migration/scope".into()),
                origin_conversation: Some("migration/origin".into()),
                source_user: Some("migration-user".into()),
            },
            access_policy: AccessPolicy::Restricted {
                allowed: vec!["migration-agent".into()],
            },
            created_at: now,
            updated_at: now,
            record_revision: 0_i64,
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::Semantic,
            importance: Importance::new(0.8_f64),
            confidence: Confidence::new(0.9_f64),
            impression_count: 2_u64,
            last_impressed_at: Some(fixed_time(3_u32)),
            superseded_by: None,
            activity_mass: 1.25_f64,
            last_used_at: Some(fixed_time(4_u32)),
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    fn fixed_time(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026_i32, 5_u32, 10_u32, 12_u32, minute, 0_u32).single().unwrap()
    }

    fn sqlite_embedding_blob(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_ne_bytes()).collect()
    }

    fn sqlite_options(path: &Path, dry_run: bool, yes: bool) -> SqliteToPostgresOptions {
        SqliteToPostgresOptions {
            sqlite_path: path.to_path_buf(),
            postgres_url: postgres_smoke_url(),
            embedding_dimensions: TEST_EMBEDDING_DIMENSIONS,
            batch_size: DEFAULT_BATCH_SIZE,
            dry_run,
            yes,
        }
    }

    async fn open_postgres_migration_store() -> PostgresStore {
        let config = PostgresDatabaseConfig {
            url: postgres_smoke_url(),
            max_connections: 1_u32,
            migration_lock_timeout_secs: 5,
            auto_migrate: true,
        };
        PostgresStore::open(&config, TEST_EMBEDDING_DIMENSIONS).await.unwrap()
    }

    async fn reset_postgres_migration_database() {
        assert_destructive_postgres_smoke_allowed();
        let store = open_postgres_migration_store().await;
        let _result = query(
            "
            TRUNCATE TABLE
                context_audit_events,
                context_anchor_overrides,
                context_kind_policies,
                memory_contexts,
                context_relations,
                context_grants,
                context_resolver_hints,
                context_identities,
                context_aliases,
                contexts,
                memory_audit_log,
                memory_tombstones,
                memory_metadata,
                memory_entities,
                memory_embeddings,
                embedding_profile,
                memories
            RESTART IDENTITY CASCADE
            ",
        )
        .execute(store.pool())
        .await
        .unwrap();
    }

    #[test]
    fn sqlite_fts_contract_accepts_bracket_and_backtick_option_values() {
        validate_sqlite_fts_external_content("CREATE VIRTUAL TABLE memory_fts USING fts5(content, content=[memories], content_rowid=`rowid`)").unwrap();
    }

    #[test]
    fn sqlite_fts_argument_splitter_closes_brackets_at_first_terminator() {
        let arguments = split_sqlite_fts_arguments("[decoy]], content=memories, content_rowid=rowid");

        assert_eq!(arguments, ["[decoy]]", " content=memories", " content_rowid=rowid"]);
    }

    #[test]
    fn sqlite_fts_contract_ignores_options_inside_bracketed_identifier() {
        let error = validate_sqlite_fts_external_content("CREATE VIRTUAL TABLE memory_fts USING fts5(content, [decoy, content=memories, content_rowid=rowid, x])").unwrap_err();

        assert!(error.to_string().contains("external-content"));
    }

    fn corrupt_sqlite_source<F>(path: &Path, mutate: F)
    where
        F: FnOnce(&Connection) -> rusqlite::Result<()>,
    {
        SqliteStore::register_extension().unwrap();
        let conn = Connection::open(path).unwrap();
        mutate(&conn).unwrap();
    }

    async fn drop_postgres_migration_schema() {
        assert_destructive_postgres_smoke_allowed();
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let _result = query(
            "
            DROP TABLE IF EXISTS
                context_audit_events,
                context_anchor_overrides,
                context_kind_policies,
                memory_contexts,
                context_relations,
                context_grants,
                context_resolver_hints,
                context_identities,
                context_aliases,
                contexts,
                context_kinds,
                memory_audit_log,
                memory_tombstones,
                memory_metadata,
                memory_v2_metadata,
                memory_entities,
                memory_embeddings,
                embedding_profile,
                memories,
                scope_registry,
                localhold_migrations
            CASCADE
            ",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    #[test]
    fn sqlite_fts_contract_accepts_quoted_module_names() {
        for module in ["\"fts5\"", "`fts5`", "[fts5]"] {
            validate_sqlite_fts_external_content(&format!("CREATE VIRTUAL TABLE memory_fts USING {module}(content, content=memories, content_rowid=rowid)")).unwrap();
        }
    }

    async fn drop_postgres_constraint(case: &MissingManagedKeyCase) {
        let pool = open_postgres_pool(&postgres_smoke_url()).await.unwrap();
        let cascade = if case.cascade { " CASCADE" } else { "" };
        let statement = format!("ALTER TABLE {} DROP CONSTRAINT {}{}", case.table, case.constraint, cascade);
        let _result = query(AssertSqlSafe(statement)).execute(&pool).await.unwrap();
    }

    fn postgres_smoke_url() -> String {
        std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into())
    }

    fn assert_destructive_postgres_smoke_allowed() {
        let allowed = std::env::var("LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE").is_ok_and(|value| value == "1");
        assert!(allowed, "destructive PostgreSQL smoke cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1");
    }
}
