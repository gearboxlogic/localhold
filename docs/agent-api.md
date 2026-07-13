# Agent API

LocalHold is the default agent-facing MCP surface. It keeps the first-use
workflow small while leaving maintenance and repair operations under explicit
`admin_*` tools.

Legacy `memory_*` tools are no longer registered as MCP tools. New agents should
start with `brief`, `recall`, `read` or `read_many`, and `remember`.

## Recommended Workflow

1. Call `brief` with a task query and any available `context_hints`.
2. Use `recall` for focused searches. It returns compact cards only.
3. Call `read` or `read_many` on card IDs that need full content.
4. Use `remember` or `remember_many` for durable decisions, lessons, or handoff-worthy context.
5. Use `handoff` before ending a session to validate candidate memory bullets.
6. Use `revise` or `forget` only when correcting or removing existing memories.

The server is deterministic. It validates, ranks, stores, and retrieves memory,
but it does not generate summaries with an LLM. Agents supply summaries and
candidate writes.

## Core Tools

### `brief`

Returns deterministic structured context grouped as `relevant`, `decisions`,
`wip`, `lessons`, `stale_candidates`, and `suggested_reads`.
The `decisions`, `wip`, and `lessons` groups are derived from matching
`decision`, `wip`, and `lesson` tags.

Inputs:

- `query`: optional task or topic text
- `scope`: optional explicit scope key or alias
- `context_hints`: optional path, git, or workflow hints for scope resolution
- `limit`: maximum relevant cards

Use `suggested_reads` with `read_many` when several IDs need full content, or
`read` for a single ID.
Responses also include `recommended_actions`, a deterministic list of next tool
calls. The server only includes `arguments` when it can construct a valid call,
such as `read`, `read_many`, or a weak-result `recall`; scope registration and
new-memory recommendations are reason-only.

### `recall`

Searches visible memories and returns compact cards. Full content is omitted by
design; call `read` with an `id` to fetch it.

Inputs:

- `query`: required search text
- `limit`: maximum cards
- `scope`: optional explicit scope key or alias
- `context_hints`: optional hints for scope resolution
- `tags`: optional tag filter
- `entity`: optional entity-name filter
- `include_weak`: include weak matches that are suppressed by default
- `search_mode`: optional explicit mode, such as `auto`, `semantic`, `keyword`,
  or `text`
- `literal_terms`: optional exact identifiers or terms for keyword matching
- `query_context`: optional extra task/query context for retrieval

Responses include `weak_result_count`, optional `scope_resolution`, `warnings`,
and per-card `match` plus `diagnostics` objects. Agents should use `match.action` first, then
`match.quality`, then `match.score` only for tie-breaking. `diagnostics` is for
debugging retrieval behavior, not for normal agent decision-making.

Per-card `match` fields:

- `quality`: `strong`, `possible`, or `weak`
- `action`: `read`, `consider`, or `ignore`
- `score`: final query relevance on a 0.0-1.0 scale
- `score_basis`: `reranker_blend`, `retrieval`, `association`, or
  `unavailable`

Per-card `diagnostics` fields:

- `retrieval_score`: first-stage retrieval score
- `reranker_score`: cross-encoder score when reranking ran
- `reranker_blend_weight`: configured blend weight when reranker scoring ran
- `vector_distance`: L2 vector distance when available
- `ranking_score`: internal ordering score combining relevance, importance,
  freshness, activity, and confidence

Diagnostics are present only for full-access cards. Redacted cards omit raw
retrieval, reranker, vector-distance, and ranking diagnostics.

### `read`

Returns one full memory by ID plus visible metadata (`summary`, `scope`,
`agent_label`, `created_by_principal`, `quality_flags`, and
`unresolved_scope`). Redacted callers receive only fields allowed by the memory
policy: hidden content suppresses summary, hidden provenance suppresses scope
and agent label, and creator/quality metadata is full-access only. When a
trusted principal is available, it records a meaningful read activity event;
anonymous public reads return
`activity_recorded: false`.

Input:

- `id`: memory ID from a compact card or other response

### `read_many`

Returns full memories for multiple IDs in input order. Missing or unreadable IDs
return per-item `status: "not_found"` instead of failing the whole batch. Found
items use the same full-memory fields and metadata as `read`.

Input:

- `ids`: one or more memory IDs, capped by `limits.max_batch_size`

When a trusted principal is available, the server records one read activity
event for all found IDs. Anonymous public reads return `activity_recorded:
false`.

### `remember`

Stores a memory without rewriting the supplied content. Warnings are advisory:
the write can still succeed when scope is unresolved, content is large,
code-like, missing a summary, missing tags/entities, or resembles an existing
memory.

Inputs:

- `content`: required durable memory text
- `summary`: optional compact card summary
- `scope`: optional explicit scope key or alias
- `context_hints`: optional hints for scope resolution
- `agent_label`: optional provenance label; it never grants access
- `tags`, `entities`, `memory_type`, `importance`, `confidence`
- `access_policy`

Entity items may be typed objects such as `{ "name": "Alice", "type": "person" }`
or strings as shorthand for `{ "name": "...", "type": "unknown" }`.
Access policy may be `"public"` shorthand, or a structured object such as
`{ "type": "restricted", "allowed": ["agent-1"] }` or
`{ "type": "redacted", "visible_fields": ["tags"] }`.

Redacted memories whose `visible_fields` omit `content` are not discoverable by
semantic, keyword, text, duplicate-candidate, reranker, or consolidation paths
for callers who cannot see content. Hidden tags, provenance/scope, and entities
also do not satisfy filters for callers whose redacted view hides those fields.
Use `restricted` when a private allowlist should retain full content search, or
include fields in `visible_fields` only when the redacted view should remain
discoverable by those fields.

Responses include an `operation` envelope plus the new `id`, resolved `scope`,
`unresolved_scope`, `scope_resolution`, `duplicate_candidates`, and `warnings`.

### `remember_many`

Stores multiple memories with the same validation, scope resolution, duplicate
checks, and warning semantics as `remember`.

Input:

- `memories`: array of `remember` inputs, or strings as shorthand for
  `{ "content": "..." }`

Structured items use the same entity shorthand rules as `remember`.
Structured items use the same access policy shorthand rules as `remember`.

Responses include an `operation` envelope and per-memory `id`, resolved `scope`,
`unresolved_scope`, `scope_resolution`, `duplicate_candidates`, and `warnings`.

### `handoff`

Validates agent-supplied candidate memory bullets. It previews suggested writes
by default and persists normalized candidates only when `commit` is `true`;
warnings are advisory.

Inputs:

- `candidates`: candidate memories with `content`, optional `summary`, optional
  `scope`, optional `context_hints`, `tags`, `entities`, and `memory_type`
  Strings are accepted as shorthand for `{ "content": "..." }`.
- `commit`: persist accepted candidates when true

Structured candidates use the same entity shorthand rules as `remember`.

Responses include an `operation` envelope. Each suggestion includes
`scope_resolution`, `duplicate_candidates`, and `next_action`, so agents can
decide whether to commit, read an existing duplicate, or classify scope first.

### `revise`

Updates an existing memory through the server-resolved principal. It can update
content, compact card metadata, tags, access policy, importance, confidence,
entities, and scope.

Inputs:

- `id`: required memory ID
- `content`, `summary`, `agent_label`
- `scope` or `context_hints` for classification
- `tags`, `access_policy`, `importance`, `confidence`, `entities`

Replacement `entities` use the same entity shorthand rules as `remember`.
Replacement `access_policy` uses the same `"public"` shorthand or structured
policy object rules as `remember`.

When content changes, embeddings are regenerated in the background.

### `forget`

Deletes a memory by ID through the server-resolved principal.

Deletion preserves a minimal authorization tombstone so `admin_history` can
authorize post-delete telemetry. The tombstone retains only the memory ID,
provenance, access policy, deletion time, and deleting principal; it does not
retain content, tags, entities, embeddings, summaries, or other recallable data.

Input:

- `id`: required memory ID

## Identity And Authorization

Authorization identity is resolved by the server, not by caller-supplied labels.

Resolution order:

1. HTTP fixed identity after endpoint bearer authentication, or a verified
   identity header when explicit trusted-proxy mode is enabled
2. stdio launch-time principal from config
3. anonymous policy

When `server.http_auth_token` is configured, it gates the entire HTTP MCP
endpoint, including initialization and tool discovery. Without that token
setting, HTTP requests are anonymous and governed by `anonymous_policy`; they
never inherit the stdio launch principal.

In the default `fixed` HTTP mode, caller-supplied principal headers are ignored
and every valid endpoint token maps to `server.http_principal`. The
`trusted_proxy` mode accepts `x-localhold-principal` only for deployments where
an authenticating proxy overwrites that header and direct access to LocalHold is
blocked.

`agent_label` and MCP client metadata are provenance only. They never grant
write, update, delete, admin, or restricted-read access.

Stdio is a single trusted-principal mode: every client connected to one stdio
server instance shares the configured `server.principal`. Real multi-agent
isolation requires distinct trusted principals, usually by running separate
stdio server instances or by using explicitly configured trusted-proxy HTTP
mode behind a verified identity layer.

The default anonymous policy is `public_read_only`. Anonymous public reads can
retrieve visible content but do not record activity ranking events because no
trusted principal is available.

## Errors And Warnings

Handler-owned tool errors are returned in-band with `is_error=true` and JSON
text:

```json
{
  "error": {
    "code": "not_found",
    "field": "id",
    "message": "memory not found: ...",
    "suggested_fix": "Check the memory ID or use recall to find a visible memory.",
    "retryable": false
  }
}
```

Error codes are `invalid_params`, `not_found`, `access_denied`,
`anonymous_read_denied`, `anonymous_write_denied`, `unavailable`, `conflict`,
and `internal`. Low-level JSON deserialization failures can still be protocol
errors before a handler runs.

Warnings keep stable `code` values and now include `severity`, optional `field`,
`message`, and optional `suggested_fix`. `quality_flags` stores warning codes
only.

## Scopes

Scopes are retrieval and write context, separate from identity. Register scopes
with `admin_scope_register`; list them with `admin_scope_list`.

A scope definition contains:

- `scope_key`
- `display_name`
- optional `description`
- `aliases`
- `matchers`
- optional `parent`
- `related`

Write scope resolution:

1. explicit `scope`, resolved as a registered key, alias, matcher-containing value,
   or raw scope key
2. registered matchers from `context_hints`
3. `inbox/unresolved`

Unresolved writes are accepted with warnings so agents can classify them later
with `revise`.

`recall` and `brief` use an explicit scope or resolved context when supplied.
When neither is supplied, they search all visible memories and cards include
scope labels. If context hints are supplied but do not match a registered scope,
the response warns and still searches all visible memories.

Responses that resolve scope include `scope_resolution` with the resolved
`scope`, `unresolved_scope`, `resolved_by` (`explicit`, `alias`, `matcher`, or
`unresolved`), and optional `matched_hint`/`matched_value` fields.

## Redaction Security Behavior

Redaction is applied as an access-controlled view across reads, search,
diagnostics, metadata, and audit/history surfaces. The behavior changed as a
breaking security fix: hidden redacted content and hidden redacted metadata no
longer make memories discoverable to unauthorized callers.

| Surface | Before | After |
|---------|--------|-------|
| `recall` | Hidden content, embeddings, tags, scope, or entities could still influence matching or filters. | Hidden content is not searched by semantic, keyword, text, duplicate, reranker, or consolidation paths; hidden tags, scope/provenance, and entities do not satisfy filters. |
| `read` | Redacted content could be hidden while server-added metadata or diagnostics could still reveal hidden context. | Redacted callers receive only policy-visible memory fields and visible metadata. Hidden summary, scope, agent label, creator, quality flags, and diagnostics stay omitted. |
| `read_many` | Per-item reads had the same redacted metadata risks as `read`. | Each found item uses the same redacted view as `read`; unreadable items remain `not_found`. |
| `admin_list` | Inventory views could reveal hidden metadata or match hidden fields through filters. | Inventory rows and filters obey the caller's visible field set. |
| `admin_history` | Deleted or redacted history could reveal principal/details to callers without full access. | Live-memory and tombstone authorization fail closed; redacted views omit principal and details. |

To keep redacted memories content-searchable for unauthorized callers,
explicitly include `content` in `visible_fields`. Use `restricted` when only the
owner and an allowlist should retain full content search. In stdio deployments,
all clients connected to the same server instance share one trusted principal, so
redaction does not isolate those clients from each other.

## Admin Tools

Admin tools are disabled by default. Setting
`server.admin_tools_enabled = true` explicitly adds these privileged
`admin_*` routes to discovery and dispatch:

- `admin_scope_register`
- `admin_scope_list`
- `admin_list`
- `admin_migration_report`
- `admin_migrate_metadata`
- `admin_count`
- `admin_history`
- `admin_reembed`
- `admin_consolidate`
- `admin_bulk_update`
- `admin_bulk_delete`
- `admin_reassign_scope`
- `admin_cleanup_expired`

Enable them only for an operator-controlled maintenance instance. Admin tools
use the server-resolved principal. `admin_list` and
`admin_scope_list` are read-like and follow the configured anonymous read
policy. Migration reporting and repair tools require a write-capable principal
because they expose whole-store maintenance state.

Admin inventory filters use the same agent-facing vocabulary as core tools:

- `agent_label`: creator provenance label, useful for diagnostics but not
  authorization
- `scope`: one explicit scope key, alias, or matcher-containing value
- `scopes`: any-match list of scope keys, aliases, or matcher-containing values
- `origin_scope`: optional historical origin scope filter for migrated or
  reassigned memories

Mutation and maintenance responses use an action-oriented `operation` envelope:

- `status`: `applied`, `partial`, `preview`, `queued`, `no_op`, `denied`, or
  `not_found`
- `changed`, plus optional `matched`, `denied`, and `capped`
- `warnings`
- `next_action`

`admin_history` exposes transactional mutation audit history for visible
memories. Mutations that require audit fail and roll back if their audit row
cannot be inserted. Redacted history views omit principal and details.
For deleted memories, history is authorized by the deletion tombstone; legacy
deleted memories or manually purged tombstones return empty history.

## Legacy Metadata Migration

Existing memories remain readable and recallable with preserved IDs and original
content. Metadata migration adds rows rather than rewriting memory content.

Use `admin_migration_report` first. It reports:

- migration candidates
- unresolved legacy scope candidates
- rows missing summaries
- duplicate candidates
- oversized and code-derived candidates

Then run `admin_migrate_metadata` with `dry_run: true` to preview a
non-destructive pass. A real pass inserts missing metadata rows only.

Legacy scopes are backfilled only when they exactly match a registered
`scope_key`. Other legacy rows are classified as `inbox/unresolved`.
