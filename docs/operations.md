# Operations

This guide covers configuration, privacy boundaries, backup, and recovery for
operators running LocalHold from a release archive or source. Managed service
definitions are not available during the early beta.

## Readiness diagnostics

Run `hold doctor` before starting a new installation or when startup behavior
is unclear. The command reports binary version and compiled capabilities,
configuration source and validity, filesystem and storage readiness, embedding
health according to the configured policy, and reranker model identity,
provider selection, and real inference readiness.

```sh
hold doctor
hold doctor --json
```

The default diagnostic is side-effect-conscious: it does not create a missing
database, migrate an existing schema, write provider identity, or download
reranker artifacts. It opens SQLite read-only and uses read-only PostgreSQL
queries. When reranking is enabled, an already cached or directly configured
model receives a real inference probe. To permit the normal pinned, verified
first-use download for that probe, opt in explicitly:

```sh
hold doctor --allow-downloads
```

Exit code `0` means healthy, `2` means degraded or not yet initialized, and `1`
means a required condition failed. The JSON form uses `schema_version: 1` and
includes the same status and exit code. It does not serialize configuration,
credentials, memory content, PostgreSQL URLs, or provider error bodies.

Examples of degraded results include a missing SQLite database that normal
startup would create, a schema that needs a normal startup migration, an
unavailable optional embedding endpoint, or an enabled reranker whose model is
not cached when downloads were not allowed. Corrupt storage, unreachable
required storage, invalid configuration, and unavailable required reranking
are failed results.

## Configuration

The canonical configuration file is `localhold.toml` under the platform user
configuration directory:

- Linux: `~/.config/localhold/localhold.toml`
- macOS: `~/Library/Application Support/localhold/localhold.toml`
- Windows: `%APPDATA%\localhold\localhold.toml`

LocalHold does not load configuration from the current working directory.
Runtime overrides use the documented `LOCALHOLD_*` environment variables.

Start from [the example configuration](../localhold.example.toml). Restrict
config-file permissions because embedding API keys, HTTP bearer tokens, and
PostgreSQL credentials may be present.

The binary provides side-effect-conscious configuration commands:

```sh
hold config paths
hold config init
hold config validate
```

`config paths` reports the canonical path, the active file, and every searched
path without loading configuration. `config init` creates a minimal starter at
the canonical path and refuses to replace any existing path; edit or replace
an existing file explicitly. On Unix, the new file is created with mode
`0600`. `config validate` checks the effective file plus `LOCALHOLD_*`
overrides, but does not open storage, contact embedding or reranking providers,
download models, or start a transport. Validation exits `0` when valid and `1`
when invalid or unreadable; `config init` also exits `1` when creation is
refused or fails. Parser details are suppressed on validation failure because
TOML diagnostics can contain secret-bearing source lines.

All three commands accept `--json`. Their output includes `schema_version: 1`;
automation should reject unknown schema versions before consuming other
fields.

## External Compute

The default `noop` embedding provider performs text-only retrieval and sends no
memory content to a model endpoint. Selecting `openai_compatible` sends memory
content for indexing and search queries for retrieval to the configured local
or cloud endpoint. LocalHold does not start or manage that service.

Review the endpoint operator's retention, logging, residency, and access
policies before enabling it. Do not place API keys in URLs; use
`embedding.openai_compatible.api_key` or `LOCALHOLD_EMBEDDING_API_KEY`.
Provider-specific request and authentication settings are documented in
[Embedding Providers](embedding-providers.md).

The optional reranker runs in the LocalHold process. Its model and tokenizer
are downloaded into the configured cache on first use unless `model_path`
points to pre-provisioned files. The built-in model uses LocalHold-produced,
checksum-pinned fused artifacts derived from the immutable upstream revision.

Multiple LocalHold processes may share a reranker cache when they run as the
same operating-system user or otherwise have compatible directory permissions.
An already complete cache entry is verified by SHA-256 and consumed without
creating files, so pre-provisioned caches may be mounted read-only. Downloads
and repairs require a writable cache and are coordinated per model and revision
with a persistent `.download.lock` file. Artifacts are written to unique staging
files, verified against their configured SHA-256 hashes, and only then moved
into place. A crashed process releases its operating-system lock automatically;
the next process removes abandoned staging files and resumes with a fresh
download.

All processes sharing one model-and-revision cache entry must use the same
expected hashes. Do not share a writable cache between mutually untrusted
users. The cache contains public model artifacts rather than memory content or
credentials, but its owner must be able to create, replace, and remove files.
Removing an unused model-revision directory is safe while no LocalHold process
is using it; LocalHold downloads it again on demand. Each process still loads
its own model session into memory—the shared cache avoids duplicate disk and
network use, not per-process RAM or VRAM use.

### Reranker model artifacts

Use the model operator commands to separate network access from offline
integrity checks:

```sh
hold models fetch --yes
hold models verify
hold models verify --json
```

`models verify` is strictly offline. It does not create the cache directory,
repair files, initialize ONNX Runtime, open storage, or start a transport. It
resolves the configured paths and hashes both `model.onnx` and
`tokenizer.json`. Exit code `0` means both files match their configured
SHA-256 values. Exit code `1` covers missing files, unpinned direct files, hash
mismatches, unreadable files, or invalid artifact configuration.

`models fetch` is the explicit network-capable operation and refuses to run
without `--yes`. It uses the same per-cache lock, bounded downloads, unique
staging files, SHA-256 checks, and atomic publication as first-use startup,
then re-verifies both published files. An already verified cache is left
unchanged. `model_path` is operator-managed and is never downloaded or
replaced; under a direct-file configuration, `models fetch --yes` only verifies
the supplied files.

Both commands accept `--json`. Reports use `schema_version: 1`, identify
whether network access was allowed, list the resolved artifact paths and
expected hashes, and include `status` plus `exit_code`. Automation should
reject unknown schema versions. Status values are `verified`, `missing`,
`hash_mismatch`, `unverifiable`, `refused`, and `error`.

For direct files, set both `search.reranker.model_sha256` and
`search.reranker.tokenizer_sha256`; otherwise offline verification reports
`unverifiable` even when both files exist. Normal startup retains its explicit
operator-managed direct-file behavior, so adding verification pins can be
rolled out independently.

### Reranker execution providers

`search.reranker.execution_provider` controls ONNX inference placement:

- `auto` prefers CUDA in a CUDA-capable binary, but only keeps it selected when
  session construction and initial health inference succeed; otherwise it
  warns and selects CPU.
- `cpu` uses CPU even when CUDA support is compiled into the binary.
- `cuda` requires a CUDA-backed session and never falls back to a CPU session.
  ONNX Runtime may still place individual shape or control-flow nodes on CPU;
  the selected provider describes the session's accelerator, not exclusive
  placement of every graph node.

`search.reranker.required = true` makes startup fail unless the selected
provider passes initial inference. With the default `false`, LocalHold can
continue without active reranking and reports `selected=none` or `active=none`.
Startup logs report the compiled, requested, selected, and active providers.
`LOCALHOLD_RERANKER_EXECUTION_PROVIDER`, `LOCALHOLD_RERANKER_PRECISION`, and
`LOCALHOLD_RERANKER_REQUIRED` override the corresponding TOML values.

### Reranker model precision

`search.reranker.precision = "fp32"` is the default. This artifact is fused to
reduce ONNX graph overhead while retaining FP32 weights and computation. It is
the portable choice for CPU, CUDA, and `auto`; CUDA can fall back to CPU without
changing artifacts. Existing custom model and hash overrides retain their
previous upstream-download behavior.

`search.reranker.precision = "fp16"` selects a fused half-precision artifact
whose file and weight storage are approximately half the FP32 size; total VRAM
savings vary with runtime buffers and workload. It is supported only when
`execution_provider = "cuda"` is explicit. LocalHold rejects FP16
with `auto` because a CUDA failure would otherwise send the FP16 graph to CPU,
where the tested runtime was substantially slower than FP32. Custom FP16 files
may be supplied with `model_path`, but the explicit-CUDA requirement remains.

FP16 can make CUDA reranking faster, especially as the candidate count or
sequence length grows. The tradeoff is reduced numerical precision: documents
with nearly equal logits can change order or cross a result-set boundary. In a
small local comparison against the upstream FP32 graph, top-five membership was
unchanged, while two of ten queries changed one member of the top ten. That is
an indicative engineering check, not a ranking-quality benchmark. More testing
is needed across representative corpora, query types, candidate counts, CUDA
architectures, and ranking metrics before quantifying the impact reliably.
Operators choosing FP16 should evaluate recall and ranking quality on their own
golden queries and preserve FP32 as the rollback profile.

Before publishing or deploying a CUDA artifact broadly, run the
[real-GPU reranker release gate](gpu-release-gate.md). It proves selected and
active provider use with real inference, compares FP32 CPU and CUDA ordering,
and records the one/four/eight-client latency, throughput, RSS, and VRAM
thresholds. The optional FP16 run uses the same FP32 CPU baseline and emits a
separate report so its speed/resource gains never erase its ranking tradeoff.

The managed artifacts come from
`cross-encoder/ms-marco-MiniLM-L6-v2` revision
`c5ee24cb16019beea0893ab7796b1df96625c6b8`, transformed with ONNX Runtime
1.22's BERT optimizer (`opt_level=0`, 12 attention heads, hidden size 384). The
FP16 artifact adds the optimizer's float16 conversion. LocalHold keeps FP32 and
FP16 in separate cache directories and verifies both the model and tokenizer
SHA-256 before publishing either cache entry. The
[artifact release](https://github.com/gearboxlogic/localhold/releases/tag/reranker-minilm-l6-v1)
includes checksums, transformation provenance, and license notices.

## HTTP Deployment

Enable the streamable HTTP transport in the server configuration:

```toml
[server]
transport = "http"
host = "127.0.0.1"
port = 8080
path = "/mcp"
http_auth_token = "replace-with-a-secret"
http_allowed_hosts = ["localhost", "127.0.0.1", "::1"]
```

With this configuration the MCP endpoint is `http://127.0.0.1:8080/mcp`. HTTP
requests never inherit the stdio principal. Without `http_auth_token`,
requests are anonymous and the default policy allows public reads but denies
writes.

`http_allowed_hosts` rejects requests whose `Host` header is not listed, even
with a valid bearer token. Reverse-proxy and private-network deployments must
add the name or address clients actually use — for example
`localhold.internal` or the LAN IP — to this allowlist.

Bind to loopback unless a reverse proxy or private network boundary is in
place. Set `server.http_auth_token` for every non-local deployment.

`server.http_principal_mode = "fixed"` is the safe default. Every request with
the valid bearer token receives `server.http_principal`, and caller-supplied
identity headers are ignored.

Use `trusted_proxy` only when all of the following are true:

- clients cannot connect directly to LocalHold;
- the proxy authenticates each caller;
- the proxy removes any client-supplied `x-localhold-principal` header;
- the proxy writes its verified principal into that header; and
- the proxy supplies LocalHold's endpoint bearer token.

LocalHold rejects trusted-proxy requests whose principal header is missing,
empty, or invalid; it does not fall back to anonymous authorization.

Treat the streamable HTTP transport as a trusted-service deployment surface,
not as an internet-facing authentication service.

`server.http_max_sessions` bounds retained stateful MCP sessions (128 by
default). Clients should close sessions with the MCP HTTP `DELETE` flow when
finished. Abandoned sessions are reaped after
`server.http_session_idle_timeout_secs` (15 minutes by default), while active
SSE streams remain protected. `server.max_body_bytes` separately bounds each
request body.

Privileged `admin_*` tools are disabled by default. Run a dedicated maintenance
instance with `server.admin_tools_enabled = true` only while an operator needs
those routes; do not expose that instance to ordinary agent clients.

## SQLite Backup And Restore

The default database is `~/.local/share/localhold/localhold.db`. SQLite uses WAL
mode, so copying only the main database while LocalHold is running is not a
valid backup. Use the supported online command instead:

```sh
hold backup ./localhold-2026-07-14.db
hold backup ./localhold-2026-07-14.db --json
```

`hold backup` may run while LocalHold is serving requests. It uses SQLite's
online backup transaction to include committed WAL data, validates
`integrity_check`, the complete managed schema, foreign keys, vector mappings,
the on-disk schema version, and the stored embedding profile, then publishes a
self-contained file without overwriting an existing path. New Unix backup files
use mode `0600`; Windows files inherit the destination directory's ACL. The
destination directory must already exist.

When startup encounters the `memory_v2_metadata` table written by the published
beta releases, it first creates and verifies a self-contained
`localhold.db.pre-upgrade-*.bak` beside the database. The backup includes
committed WAL content and is retained whether migration succeeds or fails.
Startup holds the SQLite writer lock from before that backup through legacy
validation and migration, preventing another writer from changing the captured
schema or data between those steps. If backup creation, permissions, or
`quick_check` verification fails, startup stops before changing the legacy
table. Keep this recovery copy until representative reads, scopes, audit
entries, tombstones, metadata, and embedding checks pass.

Always validate a restore first:

```sh
hold restore ./localhold-2026-07-14.db --dry-run
hold restore ./localhold-2026-07-14.db --dry-run --json
```

The dry run copies the candidate into a private staging database, validates it
against the current configuration, and verifies that the configured database
can be exclusively coordinated. It does not replace data. If the configured
database directory does not exist yet, restore creates it before acquiring the
lease, matching normal server startup. Stop every LocalHold server using the
SQLite path before the dry run and restore. Current LocalHold binaries hold
shared OS leases for their connection lifetime, so restore refuses while any
of them remains open. Stop non-LocalHold SQLite clients too; they do not
participate in the lease protocol.

After reviewing the dry run, restore explicitly:

```sh
hold restore ./localhold-2026-07-14.db --yes
hold restore ./localhold-2026-07-14.db --yes --json
```

Before replacement, LocalHold creates a uniquely named
`localhold.db.pre-restore-*.bak` recovery snapshot. The snapshot deliberately
preserves the current database even when invalid schema or embedding metadata
is the reason for restoring, so it is a rollback and forensic artifact rather
than a second validated backup. The incoming backup must still pass every
validation check. If the current file is not SQLite-readable, LocalHold instead
retains it and any `-wal`, `-shm`, or `-journal` sidecars byte-for-byte under
the reported recovery name before replacing it. Recovery names include the
UTC timestamp, process ID, and a process-local sequence to avoid concurrent
restore collisions. Replacement uses SQLite's transactional backup API;
interruption, lock failure, or insufficient disk rolls the destination back
instead of leaving a partial database. Existing database permissions are
retained. Keep the reported recovery snapshot and any matching sidecars until
representative `read`, `recall`, access-control, and embedding checks pass,
then remove them according to the operator's retention policy.

Both commands emit stable JSON with `schema_version: 1` when `--json` is used.
Reports include the validated database schema version, embedding profile,
memory and embedding counts, byte size, replacement state, and recovery path.
These commands intentionally reject `database.backend = "postgres"`; use the
PostgreSQL-native workflow below for that backend.

## PostgreSQL Backend

PostgreSQL is opt-in. Select it in the database configuration:

```toml
[database]
backend = "postgres"

[database.postgres]
url = "postgres://localhold:password@localhost:5432/localhold"
```

`LOCALHOLD_DB_BACKEND` and `LOCALHOLD_POSTGRES_URL` override these at runtime.
`migration_lock_timeout_secs` in `[database.postgres]` bounds how long each
startup schema-migration lock acquisition waits; see
[localhold.example.toml](../localhold.example.toml) for the full PostgreSQL
configuration surface.

## PostgreSQL Backup And Restore

PostgreSQL schema migration is transactional but does not create a server-side
backup. Take and verify a managed snapshot or logical backup before starting a
new LocalHold version; a rolled-back migration is not a substitute for recovery.

Use the PostgreSQL tools that match the server version and follow the database
operator's normal encryption and retention policy. A typical logical backup is:

```sh
pg_dump --format=custom --file=localhold.dump "$LOCALHOLD_POSTGRES_URL"
```

Restore into an empty database and run LocalHold against it only after the
restore succeeds:

```sh
pg_restore --exit-on-error --clean --if-exists \
  --dbname="$LOCALHOLD_POSTGRES_URL" localhold.dump
```

Test restore procedures on a disposable database. PostgreSQL preview support
does not replace managed-service snapshots, point-in-time recovery, or access
controls.

## Migrating SQLite To PostgreSQL

To migrate an existing SQLite database into an empty PostgreSQL database:

```sh
export LOCALHOLD_POSTGRES_URL="postgres://localhold:password@localhost:5432/localhold"

hold migrate sqlite-to-postgres \
  --sqlite ~/.local/share/localhold/localhold.db \
  --embedding-dimensions 768 \
  --dry-run
```

The destination can also be passed explicitly with `--postgres-url`, or read
from a different environment variable via `--postgres-url-env`. Review the dry
run, then repeat with `--yes`. The destination must not already contain user
data.

## Recovery Checks

### Embedding status

Use the embedding-specific status command to compare the configured vector
space with the identity and progress stored in SQLite or PostgreSQL:

```sh
hold embeddings status
hold embeddings status --json
```

The command does not create or migrate a database, stamp a profile, clear
vectors, or start the MCP server. It uses read-only storage queries. For an
OpenAI-compatible provider it also performs the configured health probe;
`health_check = "disabled"` reports `check_disabled` without network access.

The stable JSON document has `schema_version: 1` and reports:

- the secret-free configured and stored provider, endpoint, model, and
  dimensions;
- provider health and physical vector-table dimensions;
- total, embedded, pending, claimed, mapped, and vector-row counts; and
- missing or unexpected vectors when flags and stored rows disagree.

API keys, PostgreSQL credentials, memory content, and provider error bodies are
never included. Because endpoint and model identity are deliberately visible,
treat status output as operational metadata rather than public telemetry.

States have the following meaning:

- `disabled`: the noop provider is selected and any existing vector table has
  compatible dimensions;
- `not_initialized`: the database schema does not exist yet;
- `ready`: empty vector storage can be stamped by normal startup;
- `rebuilding`: the stored profile matches and memories still need vectors;
- `complete`: every memory has a current vector;
- `reindex_required`: configured identity or dimensions differ from storage;
- `inconsistent`: flags, mappings, and vector rows disagree; and
- `unavailable`: storage could not be inspected safely, including missing
  PostgreSQL embedding tables when automatic migration is disabled.

Exit code `0` means healthy or intentionally disabled, `2` means initialization,
rebuild work, or provider recovery remains, and `1` means storage is
unavailable, inconsistent, or requires explicit reindexing.

## Changing Embedding Providers

LocalHold records the active OpenAI-compatible endpoint, model, and dimensions
as the identity of the stored vector space. Startup fails rather than mixing
vectors when that identity changes. After changing embedding configuration,
stop every LocalHold process connected to the store, back up the database, and
run:

```sh
hold embeddings reindex --yes
```

This removes stored vectors and in-progress embedding claims, but preserves
memory content, access policies, metadata, and audit history. Start LocalHold
normally afterward; its recovery worker rebuilds vectors through the newly
configured endpoint. A legacy database with vectors but no recorded profile
also requires this explicit transition.

Keep all other LocalHold processes stopped until the reindex command completes
and every instance has been restarted with the new embedding configuration.
Running processes also re-check the profile before vector writes and reject a
write if an operator changes the vector space underneath them.

After a restore or provider change:

- confirm the configured storage backend and embedding dimensions;
- run a text-only query before enabling semantic retrieval;
- inspect representative public, restricted, and redacted memories;
- verify HTTP identity behavior when HTTP is enabled; and
- retain the pre-change backup until application-level checks pass.
