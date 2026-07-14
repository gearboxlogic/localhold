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

Pre-release development databases containing the retired
`memory_v2_metadata` table are not compatible with the current schema. LocalHold
stops with an actionable error instead of importing or ignoring that data. Back
up and reset those databases before starting the current release.

Current SQLite databases carry `PRAGMA user_version = 1`, the first schema
contract after the pre-1.0 lineage reset. Startup migrates an otherwise
compatible unversioned database to this contract, but refuses a database whose
version is newer than the running binary. Supported backups and restores expose
and validate this value; the JSON report's separate `schema_version` identifies
the report format itself.

## Protocol Compatibility

MCP protocol versions are negotiated during initialization. Supported clients
must use a protocol version accepted by the bundled Rust MCP SDK. Tool schema
snapshots are checked in and reviewed as public API changes.

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
