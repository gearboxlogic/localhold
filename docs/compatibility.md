# Compatibility Policy

LocalHold follows Semantic Versioning. The public version stream begins at
`0.1.0-beta.1`; the `0.x` series is for compatibility and installation
rehearsal before the first stable release.

## Public Contracts

Release notes identify changes to these contracts:

- the `hold` command and exit behavior;
- `localhold.toml` and documented `LOCALHOLD_*` settings;
- MCP tool names, input schemas, response schemas, and authorization behavior;
- SQLite and PostgreSQL schemas and migrations; and
- stored embedding compatibility requirements.

During `0.x`, a release may make a breaking change, but it must document the
impact and provide a safe data migration or an explicit export/reindex path.
Once `1.0.0` is released, breaking public-contract changes require a new major
version.

## Data Compatibility

Every release must either open data from earlier public LocalHold releases or
stop with an actionable migration error. LocalHold must not silently discard,
reinterpret, or mix incompatible stored data. Downgrades are unsupported unless
a release explicitly documents a rollback procedure.

Back up the active store before upgrades, storage migrations, bulk maintenance,
or embedding-provider changes. See [Operations](operations.md).

The published `v0.1.0-beta.1`, `v0.1.0-beta.2`, and `v0.1.0-beta.3` schemas
used the table name `memory_v2_metadata`. Normal startup migrates that table
to `memory_metadata`
without replacing memory content or metadata relationships. Before mutating any
persisted managed pre-v3 database, SQLite retains a verified, mode-`0600`
pre-upgrade backup; PostgreSQL performs the schema change in its migration
transaction and relies on the operator's required pre-upgrade snapshot. A
database containing both table names, malformed legacy metadata, or unexpected
metadata versions is refused without partial migration.

Current SQLite databases carry `PRAGMA user_version = 3`. Startup upgrades
schema-v1 and schema-v2 databases to this contract and migrates an otherwise
compatible unversioned database, but refuses a database whose version is newer
than the running binary. PostgreSQL's current migration ledger ends at version
5 (`governed_contexts`). Supported backups and restores expose and validate
these values; restore upgrades a strictly validated older backup on a private
staging copy.

The governed-context migration converts registered and raw legacy scopes into
normalized contexts and ordered `memory_contexts` rows, keeps
`inbox/unresolved` memories contextless, converts former global definitions
into frozen system-owned compatibility contexts with their prior visibility,
and derives grants for raw-only contexts from the attached memories' existing
access policies instead of exposing every historical key globally. Compatible
unversioned databases without a registry still receive this raw-scope backfill.
The migration removes `scope_registry` after successful backfill.
`origin_scope` remains provenance. Compatibility `scope` fields survive as
synchronized caches in metadata and current provenance.
The JSON report's separate `schema_version` identifies the report format
itself.

Deterministic SQLite and PostgreSQL fixtures cover every published beta, release
candidate, and stable schema. CI verifies their checksums and source provenance,
then opens or migrates every manifest entry and checks embedding profiles, audit
history, governed contexts, memberships, grants, policy, compatibility scopes,
tombstones, metadata, memories, and embeddings. Fixtures remain
for the lifetime of the release's compatibility obligation; removing one
requires an explicit, reviewed compatibility-policy change in a release.

## Protocol Compatibility

MCP protocol versions are negotiated during initialization. Supported clients
must use a protocol version accepted by the bundled Rust MCP SDK. Tool schema
snapshots are checked in and reviewed as public API changes.

The governed-context migration intentionally changes writes that omit both
`context` and legacy `scope`: `remember` and `remember_many` now return
`context_required` unless policy provides a unique safe default. Upgrade
callers by sending the context envelope, retaining a legacy `scope` during
migration, configuring a safe default, or explicitly deferring with
`{"context":{"allow_unresolved":true}}`.

The retired `admin_v2_migration_report` and `admin_v2_migrate_metadata` tool
names are not registered as aliases. Current maintenance clients must use
`admin_migration_report` and `admin_migrate_metadata`.

Security fixes may intentionally tighten authentication, authorization,
redaction, or destructive-operation behavior in a minor or patch release. Such
changes are called out prominently and are not treated as regressions to unsafe
behavior.

## Support Matrix

Current support levels are:

| Surface | Level |
| --- | --- |
| Linux x86_64 CPU, stdio, SQLite | Supported beta |
| OpenAI-compatible embeddings | Supported beta |
| CPU ONNX reranker | Supported beta |
| Streamable HTTP with SQLite | Preview |
| PostgreSQL with pgvector | Preview |
| Windows x86_64 MSVC | Preview |
| CUDA 12 reranker | Preview |
| macOS and Linux ARM64 artifacts | Deferred |

CUDA-capable builds also retain CPU support. The configured reranker policy
distinguishes compiled providers from the provider selected for the model and
the provider active after health inference; compatibility claims use the active
provider rather than the build label.

Preview surfaces receive CI or targeted validation but may require manual
configuration and may change during `0.x`. Deferred surfaces are not release
gates and should not be presented as supported.
