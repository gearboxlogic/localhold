# Operations

This guide covers configuration, privacy boundaries, backup, and recovery for
operators running LocalHold from source. Release installers and managed service
definitions are not available during the early beta.

## Configuration

The canonical configuration file is `localhold.toml` under the platform user
configuration directory:

- Linux: `~/.config/localhold/localhold.toml`
- macOS: `~/Library/Application Support/localhold/localhold.toml`
- Windows: `%APPDATA%\localhold\localhold.toml`

LocalHold does not load configuration from the current working directory.
Existing `recall.toml` files in the same user configuration directory remain a
temporary fallback when `localhold.toml` is absent. Runtime overrides continue
to use the documented `RECALL_*` environment variables.

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
`embedding.openai_compatible.api_key` or `RECALL_EMBEDDING_API_KEY`.
Provider-specific request and authentication settings are documented in
[Embedding Providers](embedding-providers.md).

The optional reranker runs in the LocalHold process. Its model and tokenizer
are downloaded into the configured cache on first use unless `model_path`
points to pre-provisioned files.

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
pg_dump --format=custom --file=localhold.dump "$RECALL_POSTGRES_URL"
```

Restore into an empty database and run LocalHold against it only after the
restore succeeds:

```sh
pg_restore --exit-on-error --clean --if-exists \
  --dbname="$RECALL_POSTGRES_URL" localhold.dump
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
