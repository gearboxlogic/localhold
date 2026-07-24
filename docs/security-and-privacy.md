# Security, Privacy, And Threat Model

This document describes LocalHold's current trust boundaries, data movement,
and residual risks. It is deployment guidance, not a claim that LocalHold can
protect data after the operating-system account, database, or configured model
provider is compromised.

For vulnerability reporting, use the private process in
[SECURITY.md](../SECURITY.md). For the current platform support levels, see the
[compatibility policy](compatibility.md#support-matrix).

## Security Goals

LocalHold is designed to:

- keep storage and text-only retrieval on the operator's machine by default;
- authorize MCP reads and writes with a server-resolved principal rather than
  a caller-provided label;
- exclude memory fields a caller cannot read from returned results and from
  reranker input;
- avoid putting API keys or database credentials in normal status reports;
- preserve database consistency across audited mutations, embedding retries,
  backups, and migrations; and
- make every optional network path an operator choice.

LocalHold does not provide application-level encryption at rest, secure
deletion, a multi-user identity provider, TLS termination, an internet-facing
authentication service, or isolation from other processes running as the same
operating-system user.

## Default Data Flow

The default deployment is one MCP client, one LocalHold process over stdio, the
`noop` embedding provider, and one local SQLite database:

```text
MCP client
  -> stdio
     -> LocalHold process
        -> SQLite database and sidecars
```

That path does not send memory content, search queries, or usage telemetry over
the network. Semantic embeddings, PostgreSQL, streamable HTTP, and reranking
are opt-in:

```text
MCP client
  -> stdio or HTTP
     -> LocalHold process
        -> SQLite file or PostgreSQL connection
        -> OpenAI-compatible embedding endpoint (optional)
        -> in-process ONNX reranker (optional)

LocalHold process
  -> model artifact host on first reranker use (optional)
```

"Local" describes where a configured component runs, not a different privacy
contract. A model server on loopback, a server on a private LAN, and a cloud API
receive the same embedding request fields. The operator is responsible for the
endpoint's access controls, retention, logging, residency, and subprocesses.

## Data Inventory

### Stored data

SQLite and PostgreSQL store the following logical data:

- memory content, tags, type, confidence, importance, timestamps, and expiry;
- provenance, source and creator principals, access policies, and allowlists;
- summaries, scopes, aliases, matchers, quality flags, and entity names/types;
- supersession links, impression counts, activity values, and embedding work
  claims;
- derived full-text indexes and embedding vectors;
- embedding profile metadata: provider, endpoint, model, and dimensions;
- mutation audit rows, including caller principals and structured details; and
- deletion tombstones containing identifiers, policy/provenance, and deletion
  time, but not deleted memory content; ordinary deletes also record the
  deleting principal.

Raw search query text is not stored as search history. Agent-facing search can
increment impression counts and timestamps for returned memories. A semantic
query and optional query context are still sent to the configured embedding
endpoint and are held in process while the request runs.

Write-time duplicate checks can send a candidate content excerpt to that
endpoint before the memory is committed. This includes `remember`,
`remember_many`, and a preview-only `handoff` with `commit = false`. A preview
can therefore disclose candidate text to the provider even though LocalHold
does not persist it.

Embeddings are derived from content and may reveal information about it. Treat
vectors, full-text indexes, audit rows, scope metadata, and entity names as
sensitive even when they do not contain the original sentence verbatim.

### Plaintext at rest

LocalHold does not encrypt database columns or files. SQLite databases, WAL and
shared-memory sidecars, backups, restore stages, and retained pre-upgrade or
pre-restore recovery copies contain plaintext or derived sensitive data.
PostgreSQL tables, WAL, replicas, snapshots, and logical backups are likewise
controlled by the database operator rather than encrypted by LocalHold.

Use full-disk, filesystem, volume, or managed-database encryption as appropriate.
Restrict the database directory, backup destinations, PostgreSQL roles, and
operator accounts. On Unix, LocalHold creates a missing platform-default data
directory with mode `0700` and new SQLite database, backup, and coordination
lock files with mode `0600`. It does not change permissions on existing
databases or directories, including custom database directories. SQLite
sidecars remain subject to SQLite and the filesystem. `hold doctor` reports
group/other permission bits on an existing configuration file, SQLite database,
WAL or shared-memory sidecar, or default data directory without changing them.
Windows files inherit directory ACLs.

Anyone with direct database or backup access bypasses LocalHold's MCP access
policies. Do not treat a `restricted` or `redacted` memory as encryption.

### Deletion, expiry, and supersession

Deleting a memory removes its active content, metadata, entities, and embedding
rows and retains a minimal tombstone so later audit-history reads can still be
authorized. TTL expiry alone only hides a memory from ordinary reads and
searches; the content and derived data remain stored until an enabled
`admin_cleanup_expired` operation removes expired rows. That whole-store cleanup
requires a write-capable principal, but it does not apply each memory's policy.
Every cleanup deletion records the server-resolved principal in its tombstone
and a transactional per-memory delete audit row. Audit rows and tombstones have
no automatic retention limit. Supersession is not deletion: the older memory
remains stored and can be requested explicitly.

Database deletion is not secure erasure. Deleted values can remain in SQLite
free pages or WAL, PostgreSQL MVCC/WAL and replicas, snapshots, logs, and older
backups. Apply an operator-owned retention and destruction policy to databases,
sidecars, recovery copies, logs, and backups.

## Network And Compute Boundaries

### Runtime and operator network paths

| Path | Trigger | Data crossing the boundary | Controls and residual risk |
| --- | --- | --- | --- |
| OpenAI-compatible `POST /embeddings` | `embedding.provider = "openai_compatible"` | Memory content during indexing; candidate excerpts during pre-commit duplicate checks, including preview-only handoff; search query and optional query context during semantic search; model name; optional dimensions; API key header | Non-loopback endpoints require HTTPS unless `allow_insecure_http = true`. Redirects are disabled. The endpoint can retain or log inputs even when LocalHold does not commit a candidate. |
| OpenAI-compatible `GET /models` | Default health check during provider initialization/recovery, `hold doctor`, `hold embeddings status`, and re-embed or TUI paths that initialize the provider | API key header and normal HTTP metadata; no request body, configured model name, memory content, or search query | The configured model is compared locally with the response. Set `health_check = "disabled"` only when the endpoint lacks model listing. Embedding calls still occur. |
| System HTTP proxy | Reqwest discovers `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, and lowercase equivalents for embedding and model-download requests | The proxied request, including embedding content and API key headers; proxy URL credentials when configured | LocalHold does not disable environment proxy discovery. Audit the process environment and set `NO_PROXY` for local/private endpoints that must not transit a proxy. Treat the proxy as another content and credential boundary. |
| PostgreSQL | `database.backend = "postgres"`, diagnostics, status, or migration | All persisted logical data, queries/filters, vectors, and database credentials | PostgreSQL is preview. The current build has no PostgreSQL TLS implementation. Default/preferred TLS behavior can fall back to plaintext; modes that require or verify TLS fail to connect rather than encrypt. Keep it on a trusted local boundary or use an operator-managed encrypted tunnel. |
| Streamable HTTP listener | `server.transport = "http"` | MCP requests and responses, including memory content and queries; bearer token and proxy principal header when configured | LocalHold serves plaintext HTTP. Keep the listener on loopback behind a trusted proxy, protect both the client-to-proxy and proxy-to-LocalHold hops, and prevent direct backend access. HTTP is preview and is not an internet-facing authentication service. |
| Reranker artifact download | First enabled reranker startup, `hold models fetch --yes`, or `hold doctor --allow-downloads` without a complete cache | Public model/revision identifiers and normal HTTP metadata; no memory content, query, or LocalHold credential | Built-in artifacts are checksum-pinned and verified before use. Initial hosts are GitHub Releases for built-in artifacts and Hugging Face for configured model/revision downloads; the download client can follow redirects to artifact hosts. Direct `model_path` files remain operator-managed. Pre-provision artifacts for an offline runtime. |
| In-process ONNX reranker | `search.reranker.enabled = true` | Query and already-authorized candidate content remain inside the LocalHold process | No reranking API exists. Model dependencies and process memory share the LocalHold trust boundary. |
| PostgreSQL backup tools | Operator runs `pg_dump`, `pg_restore`, or managed snapshot tooling | Complete database contents and database credentials | Encryption, access, destination, and retention are operator responsibilities. Prefer environment or protected credential files over command-line URLs. |

Embedding requests can be batched and concurrent. Several LocalHold processes
using one endpoint have independent concurrency limits and can disclose or bill
for duplicate work after a claim expires, although revision checks prevent a
stale vector from replacing current data. Size provider limits for aggregate
traffic.

Successful embedding response bodies are limited to 16 MiB and successful
model-list responses to 1 MiB. LocalHold enforces both limits while streaming,
including when the provider omits `Content-Length`. Provider HTTP error bodies
are discarded rather than included in runtime errors or logs; LocalHold retains
the HTTP status, operation context, retry classification, and valid
`Retry-After` delay. These controls do not replace provider timeouts,
concurrency limits, or aggregate network controls.

Intermediaries have their own logging boundaries. Configure reverse proxies,
WAFs, and access logs not to record authorization headers or MCP request/response
bodies. Configure PostgreSQL statement, parameter, connection, and error logging
with the understanding that content, filters, queries, identifiers, or database
credentials can otherwise reach database logs. These logs are outside
LocalHold's redaction controls.

### Build, release, and maintenance network paths

These paths are not contacted by a default running `hold` process:

- Cargo, rustup, mise, GitHub Actions, and dependency automation contact their
  normal registries and GitHub during development or CI.
- PostgreSQL CI can pull the digest-pinned `pgvector` image from Docker Hub when
  it is not already available on the runner.
- CUDA release preparation downloads checksum-pinned public packages from
  package hosting before assembling an artifact.
- GitHub release workflows upload artifacts and publish releases with the
  job-scoped ephemeral GitHub token. Maintainers separately use their Git/GitHub
  credentials to push a release tag, and GPU release-gate operators need a
  GitHub credential when registering a repository-scoped runner. The repository
  currently has no runtime release credential, signing key, registry token, or
  update-check service.

Release archives and `SHA256SUMS` are published together through the same
GitHub release. The checksum detects corruption or a mismatched download, but
it is not an independent signature or provenance channel and does not protect
against compromise of a maintainer, repository, tag, workflow, runner, or
GitHub release account. Current releases do not provide signed archives, a
software bill of materials, or artifact attestations. Deployments that require
independent provenance should review and pin the source commit and build the
locked source in a trusted pipeline.

LocalHold has no runtime analytics, crash-reporting, update-check, or remote
reranking path. Model artifacts and embedding requests are the only HTTP client
traffic in the running binary; PostgreSQL is the other optional outbound
connection.

## Identity And Authorization Boundaries

Access policy is enforced by the application. Scope and `agent_label` are
categorization/provenance fields, not authorization identities.

### Stdio

One stdio server process has one launch-time `server.principal` (`stdio` by
default). Every client that can use that process shares the principal and its
read/write authority. Run separate processes with separate OS/process access or
use a correctly isolated trusted proxy when clients must have distinct
identities.

### HTTP fixed mode

The bearer token authenticates access to the whole MCP endpoint. In the default
`fixed` mode, every valid token maps to one `server.http_principal`; it is an
endpoint credential, not a per-user credential. Caller-supplied identity headers
are ignored.

Without `server.http_auth_token`, HTTP requests are anonymous. The default
`public_read_only` policy permits reads of public memories and denies writes.
`deny_all` blocks anonymous agent API access. `public_read_write` maps every
unauthenticated caller to the same synthetic `anonymous` principal; do not use
it across an untrusted or multi-user boundary.

The binary permits a non-loopback bind without a token. Configuration validity
is not a safety approval: operators must keep unauthenticated HTTP on loopback.
`http_allowed_hosts` reduces Host-header and DNS-rebinding risk but is not
authentication or transport encryption.

### HTTP trusted-proxy mode

`trusted_proxy` accepts the configured principal header only after the endpoint
bearer token passes. LocalHold does not verify proxy source IPs or authenticate
the principal header itself. A missing, blank, or invalid principal header is
rejected instead of becoming an anonymous request. The deployment is safe only
when:

- clients cannot connect directly to LocalHold;
- the proxy authenticates every caller;
- the proxy removes any client-supplied principal header;
- the proxy writes the verified principal and endpoint token on every MCP
  request;
- clients use TLS or another protected transport to the proxy;
- the proxy-to-LocalHold hop stays on loopback or uses an equivalently protected
  and isolated transport; and
- the proxy limits requests, connections, and failed authentication attempts.

HTTP sessions are process-local and stateful, but identity is resolved from
each request rather than bound permanently to a session. Apply proxy controls
to initialization, regular requests, resumed streams, and session deletion.
Horizontally scaled replicas and shared HTTP session storage are unsupported.

See [HTTP deployment](operations.md#http-deployment) for configuration and
[Identity And Authorization](agent-api.md#identity-and-authorization) for the
policy behavior.

### Local TUI

`hold ui` opens the configured store directly. Its `--principal` option and
`server.principal` setting are trusted local assertions used for policy
evaluation, not authentication. A user who can run the TUI with the database
credential can choose another principal, and direct database access bypasses
LocalHold policy entirely. Protect the process, configuration, and database
with operating-system and database controls; do not use the TUI principal as a
multi-user isolation boundary.

### Search authorization and noninterference

LocalHold applies access policy and field redaction before returning candidate
records or sending them to the optional reranker. SQLite and PostgreSQL
nevertheless generate and preliminarily rank full-text and ANN candidates
against shared store indexes before application policy filtering. Duplicate
detection and consolidation also use shared ANN candidate structures.

Inaccessible rows can therefore affect query work, timing, preliminary ranks,
pagination, and hard scan or candidate ceilings. Over-fetching and retrying
reduce crowding but do not provide constant-time behavior or policy
noninterference. Do not treat search result shape or timing as an authorization
boundary, and place mutually hostile tenants in separate stores and processes.

### Admin tools

Admin routes are absent from discovery and dispatch unless
`server.admin_tools_enabled = true`. Enabling them does not create a separate
admin role. Run a dedicated, temporary maintenance instance that ordinary
agents cannot reach. Capabilities have different authorization scopes:

| Capability | Tools | Authorization and reach |
| --- | --- | --- |
| Policy-filtered reads | `admin_list`, `admin_history` | Return only memories or history visible to the server-resolved principal. Redacted history omits principal and details. |
| Mixed-scope statistics | `admin_count` | Memory breakdowns are policy-filtered, but expired-row count and physical database size are store-wide diagnostics. |
| Global scope registry | `admin_scope_list`, `admin_scope_register` | Listing returns every registered scope to a read-allowed caller. Registration requires a write-capable principal but can replace any scope definition; scopes have no per-scope owner policy. |
| Policy-checked memory changes | `admin_bulk_update`, `admin_bulk_delete`, `admin_reassign_scope`, `admin_consolidate`, single-ID `admin_reembed` | Require a write-capable principal and check write access for affected memories. Shared ANN candidate work can still be influenced by other rows. |
| Whole-store embedding queue | bulk `admin_reembed` | Requires a write-capable principal at the route, then claims unembedded rows without per-memory authorization and can send their content to the configured provider. |
| Whole-store expiry cleanup | `admin_cleanup_expired` | Requires a write-capable principal and records it in a tombstone and transactional delete audit row for every removed memory, but still deletes all expired rows without per-memory policy checks. |
| Whole-store metadata maintenance | `admin_migration_report`, `admin_migrate_metadata` | Restricted to a local, authenticated stdio context. Reporting exposes whole-store state; migration can add metadata across the store. |

Enabling admin routes should therefore be treated as granting maintenance
capabilities, not merely exposing policy-filtered variants of normal tools.

## Secrets And Operational Metadata

| Secret | Configuration paths | Guidance |
| --- | --- | --- |
| Embedding API key | `embedding.openai_compatible.api_key`, `LOCALHOLD_EMBEDDING_API_KEY` | Use the header setting, never URL credentials. Use HTTPS outside loopback. Scope and rotate the key at the provider. |
| HTTP proxy credential | Userinfo in an operator-supplied `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, or lowercase equivalent | Protect process environment and proxy configuration. Use `NO_PROXY` to keep local/private embedding and artifact endpoints off the proxy when required. |
| HTTP bearer token | `server.http_auth_token`, `LOCALHOLD_HTTP_AUTH_TOKEN` | Treat it as authority for the whole endpoint. Use a high-entropy value, TLS at the proxy, and rotation appropriate to every client. |
| PostgreSQL credentials | `database.postgres.url`, `LOCALHOLD_POSTGRES_URL`, migration URL option/environment selection; SQLx-supported `PGPASSWORD`, `PGPASSFILE`, or default PostgreSQL passfile lookup | Protect config, environment, and passfile access. A URL password overrides `PGPASSWORD`; passfiles are consulted only when neither supplies a password. An unusable/nonmatching custom passfile can fall through to the default passfile. A URL passed directly as a CLI argument may appear in shell history or process listings. Validate passfile syntax before startup: SQLx warnings for malformed entries can include the original line and password. |
| Release credentials | Job-scoped GitHub token in release automation; maintainer Git/GitHub authentication for tag pushes; temporary GitHub authentication for repository-scoped runner registration | No long-lived release secret is configured in the repository. Protect maintainer credentials, GitHub environments, runner registration, and workflow permissions; future signing or registry secrets require a separate threat-model update. |

Normal configuration loading uses only the platform user configuration file and
documented `LOCALHOLD_*` variables, not a file in the current directory. The
SQLite-to-PostgreSQL migration command can explicitly select another environment
variable with `--postgres-url-env`. `hold config init` creates a Unix file with
mode `0600`. LocalHold does not repair permissions on an existing file; `hold
doctor` reports permissive Unix configuration and SQLite storage modes as
degraded. Environment variables and process arguments are visible according to
operating-system process-inspection rules.

Focused `Debug` implementations for the embedding provider and PostgreSQL
database config suppress their API key or URL, and diagnostic JSON omits
credentials, content, and provider error bodies. This is not a general promise
that arbitrary Rust debug output is secret-safe: the wider server config and
migration option structures can contain an HTTP token or PostgreSQL URL. Do not
log configuration structures. Logs can still include database/cache paths,
endpoint and model identity, memory and session identifiers, principals, and
sanitized provider status/context or network error text. Invalid PostgreSQL
vector rows and some migration errors can include a complete derived vector.
SQLx warnings can include malformed passfile lines or unrecognized
connection-URL parameter values, so never put secrets in unknown URL
parameters. Malformed TOML errors can include source context. Keep stderr and
service-manager logs private, control `RUST_LOG`, and do not publish diagnostics
without review.

## Shared Local Resources

### Multiple SQLite processes

SQLite uses WAL mode, a busy timeout, and transactional writer serialization.
Multiple LocalHold processes can share one database, but writer contention can
still fail and every process independently schedules embedding and provider
work. LocalHold processes hold a shared OS lock that prevents supported restore
while any of them remains open. Non-LocalHold SQLite tools do not participate
in that restore lock.

Stop every LocalHold and other SQLite process before restore, reindex, or direct
database maintenance. Never copy only the main SQLite file while a process is
running; use `hold backup`. See
[SQLite Backup And Restore](operations.md#sqlite-backup-and-restore).

### Shared model cache

The reranker cache stores public model/tokenizer artifacts, not memory content,
queries, or credentials. LocalHold coordinates downloads, verifies configured
hashes, and publishes complete files atomically. Several processes may share a
cache when their OS permissions and expected hashes agree.

Do not share a writable model cache between mutually untrusted users. Direct
`model_path` files are operator-managed, and a writable peer can cause denial
of service or substitute files unless the operator pins and verifies them. A
pre-provisioned, verified read-only cache is the lowest-network deployment.

## Deployment Classification

| Deployment | Status | Security boundary |
| --- | --- | --- |
| Linux x86_64 CPU, stdio, SQLite, `noop` embeddings | Supported beta and default | One trusted OS user and one shared stdio principal; no runtime network egress. |
| Local TUI | Supported beta | Direct database client. Principal selection is a trusted local assertion, not authentication. |
| OpenAI-compatible embeddings | Supported beta | Local, LAN, or cloud endpoint receives memory content and semantic queries. |
| CPU reranker | Supported beta | In-process inference; optional model download can be pre-provisioned. |
| Streamable HTTP with SQLite | Preview | Loopback or trusted proxy/private network only; plaintext server and process-local sessions. |
| PostgreSQL with `pgvector` | Preview | Database network, roles, encryption, backup, and retention are operator-owned. |
| Windows x86_64 and CUDA reranking | Preview | Platform/runtime limitations in the compatibility policy still apply. |
| macOS and Linux ARM64 release artifacts | Deferred | Not release-gated or supported artifacts. |
| Direct internet-facing HTTP, untrusted proxy headers, or unauthenticated non-loopback HTTP | Unsupported deployment | LocalHold is not an identity provider, TLS terminator, or internet edge. |
| Horizontally scaled HTTP replicas | Unsupported | Sessions and limits are process-local; no shared session architecture exists. |
| Untrusted users sharing a stdio process or writable model cache | Unsupported isolation model | They share authority or writable process resources. |
| LocalHold-managed encryption or secure erase | Not provided | Use OS, database, storage, log, and backup controls. |

Preview means the surface has targeted validation but may require manual
configuration and can change during `0.x`. It does not weaken the operator's
responsibility to protect the surrounding network and storage.

## Threats, Mitigations, And Residual Risk

| Threat | Current mitigation | Residual risk / operator action |
| --- | --- | --- |
| Another MCP caller reads or changes private data | Server-resolved principals; restricted/redacted policy; candidate records are filtered and redacted before return or reranking; transactional mutation authorization | Shared stdio or fixed HTTP principals are shared authority. Direct database access and locally asserted TUI principals bypass identity guarantees. Separate trust domains. |
| Search side channel or candidate interference | Policy checks prevent inaccessible rows and fields from appearing in responses or reranker input; candidate pools are over-fetched and retried | Shared ANN and full-text indexes generate and rank candidates before policy filtering. Other rows can affect timing, preliminary ranks, pagination, and candidate ceilings. Separate mutually hostile tenants. |
| Forged trusted-proxy identity | Trusted-proxy mode requires the endpoint token and a nonempty principal header | LocalHold does not authenticate proxy origin or sign headers. Block direct access, overwrite the header at the proxy, and protect both network hops. |
| Credential or content interception | HTTPS required for non-loopback embedding endpoints; redirects disabled | The HTTP server is plaintext, and the current PostgreSQL build has no TLS implementation. Protect both HTTP hops and use an encrypted database tunnel when PostgreSQL traffic crosses an untrusted boundary. |
| Cloud provider retains sensitive data | Provider is opt-in; default `noop` sends nothing | LocalHold cannot enforce provider retention. Review contracts and avoid cloud embeddings for content that cannot leave the host. |
| Malicious provider exhausts memory or bandwidth | Successful embedding/model-list bodies have streaming size caps; provider HTTP error bodies are discarded; request timeouts and embedding concurrency are bounded | A provider can still delay responses and several processes have independent limits. Apply endpoint, egress, and aggregate concurrency controls. |
| Sensitive data appears in logs | Normal diagnostics omit credentials/content; provider HTTP error bodies are discarded; focused database/provider config types have redacted debug output | Arbitrary debug output, proxy bodies/headers, PostgreSQL statements/parameters, and operational metadata can reach logs or clients. Minimize, protect, and review every log sink. |
| A permitted writer plants malicious instructions or false memory | Stored content retains provenance and access policy; write authorization limits who can mutate an existing memory | New content is stored as supplied and may later enter an agent's context. Deny anonymous writes, isolate mutually untrusted writers, review provenance, and treat recalled text as untrusted data rather than executable authority. |
| HTTP resource exhaustion | Request-body, session-count, and idle-session limits bound some retained state | LocalHold has no general request, connection, or failed-auth rate limiter, and active streams are not idle-reaped. Enforce those limits at the proxy and monitor session capacity. |
| Database or backup theft | New default Unix data directories and new SQLite database/coordination/backup files receive owner-only creation modes; doctor reports permissive existing local paths without changing them | No application encryption, and custom or existing directory policy remains operator-controlled. Use encrypted storage, strict ACLs, protected backups, and database roles. |
| Deleted data is recovered | Active content is removed and minimal tombstones retain authorization context | Free pages, WAL, replicas, logs, and backups can retain data. Enforce external retention/destruction. |
| Malicious or replaced reranker artifact | Built-in artifacts are revision/hash pinned and verified before publication | Operator-managed direct model files and shared writable caches remain trusted inputs. Pre-provision read-only verified files. |
| Compromised release artifact or checksum manifest | Release downloads include a checksum manifest | Archives and checksums share one GitHub publication boundary and are not independently signed or attested. Pin reviewed source and use a trusted build pipeline when provenance requirements exceed that boundary. |
| Duplicate or stale embedding work | Durable claims and revision-checked vector writes | Expired claims can produce duplicate disclosure/cost. Coordinate process counts and provider limits. |
| Overprivileged PostgreSQL runtime credential | `auto_migrate = false` supports a current schema under runtime-only table and sequence grants | The default `auto_migrate = true` uses the runtime URL for DDL and requires table ownership. Separate migration and runtime credentials operationally and disable runtime auto-migration. |
| Destructive admin misuse | Admin routes are disabled by default; several memory mutations apply per-memory authorization and transactional audit behavior | There is no separate admin role. Scope registry, bulk re-embedding, expiry cleanup, statistics, and metadata migration have global or mixed reach. Isolate maintenance instances and back up first. |

## Secure Deployment Checklist

1. Choose a support level from the [compatibility matrix](compatibility.md#support-matrix); do not compose preview surfaces into an assumed supported topology.
2. Restrict the OS account, config file, data directory, model cache, logs, and backups.
3. Keep stdio clients within one trust domain or assign separate server instances and principals.
4. Keep HTTP on loopback unless an authenticating proxy or equivalent private boundary blocks direct access, protects both network hops, and enforces request, connection, and authentication-attempt limits.
5. Set an HTTP bearer token for every non-local deployment; use `trusted_proxy` only with header overwrite on every request.
6. Leave admin tools disabled on ordinary agent instances, deny anonymous writes, and isolate mutually untrusted writers.
7. Keep `noop` embeddings for fully local text retrieval. Before enabling another endpoint, decide whether memory content and queries may cross that boundary.
8. Use HTTPS for non-loopback embeddings and do not enable insecure HTTP across an untrusted network.
9. Keep PostgreSQL on a trusted boundary or encrypted tunnel. Use a separate runtime role with `auto_migrate = false` after an operator-run migration, and test protected backups.
10. Run `hold doctor`, review private logs, and verify backup/restore procedures before an upgrade or migration. With an OpenAI-compatible provider, doctor performs the configured network health check; `--allow-downloads` can also fetch reranker artifacts.
11. Treat recalled memory as untrusted input to an agent, not as executable authority, even when its provenance is recorded.
12. Define retention and destruction for audit rows, tombstones, SQLite sidecars, recovery copies, PostgreSQL WAL/snapshots, and service logs.
