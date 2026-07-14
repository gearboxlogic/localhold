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
Downloads are coordinated per model and revision with a persistent
`.download.lock` file. Artifacts are written to unique staging files, verified
against their configured SHA-256 hashes, and only then moved into place. A
crashed process releases its operating-system lock automatically; the next
process removes abandoned staging files and resumes with a fresh download.

All processes sharing one model-and-revision cache entry must use the same
expected hashes. Do not share a writable cache between mutually untrusted
users. The cache contains public model artifacts rather than memory content or
credentials, but its owner must be able to create, replace, and remove files.
Removing an unused model-revision directory is safe while no LocalHold process
is using it; LocalHold downloads it again on demand. Each process still loads
its own model session into memory—the shared cache avoids duplicate disk and
network use, not per-process RAM or VRAM use.

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
valid backup.

For a filesystem backup:

1. Stop every LocalHold process using the database.
2. Confirm no process has the database open.
3. Copy `localhold.db` and any adjacent `localhold.db-wal` and
   `localhold.db-shm` files
   as one set.
4. Preserve file ownership and permissions.
5. Restart LocalHold and verify a representative `read` and `recall` workflow.

To restore, stop LocalHold, move the current database set aside, place the
backup set at the configured path, and start with the same embedding dimensions
used by the backup. Keep the previous files until the restored store has been
verified.

## PostgreSQL Backup And Restore

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

## Recovery Checks

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
