# Architecture

LocalHold is a standalone Rust MCP server for long-term agent memory. It stores memories through a backend-neutral `MemoryStore`: SQLite with `sqlite-vec` by default, or PostgreSQL with `pgvector` when configured. It exposes the whole surface as MCP tools over stdio or HTTP.

## Request Flow

```text
MCP client
  -> LocalHoldServer
      -> LocalHoldEngine
          -> EmbeddingOrchestrator
          -> MemoryStore
              -> SqliteStore
              -> PostgresStore
```

- `LocalHoldServer` in `src/server/` owns the MCP tool handlers and request/response schemas.
- `LocalHoldEngine` in `src/engine.rs` owns validation, search orchestration, audit behavior, and write flows.
- `EmbeddingOrchestrator` in `src/embedding/orchestrator.rs` enforces the store-then-embed invariant and coordinates background embedding work.
- `SqliteStore` in `src/store/sqlite.rs` is the default persistence backend and delegates to focused store modules for CRUD, query building, search, schema, and admin work.
- `PostgresStore` in `src/store/postgres.rs` is the opt-in PostgreSQL backend with async pooling, schema bootstrap, PostgreSQL full-text search, and `pgvector` vector search.

## Core Components

### Server Layer

`src/server/mod.rs` maps MCP calls to engine operations. `src/server/params.rs` is the authoritative wire surface for request and response shapes.

The registered MCP surface is the v2 agent API: `brief`, `recall`, `read`,
`remember`, `remember_many`, `handoff`, `revise`, `forget`, and explicit
`admin_*` maintenance tools. Legacy `memory_*` tools are not registered as MCP
tools. Privileged admin routes are removed from the router by default and
require explicit operator configuration.

### Engine Layer

`src/engine.rs` is the business-logic boundary:

- input validation
- search-mode orchestration
- composite ranking
- audit writes
- re-embed and maintenance flows
- entity expansion and bulk operations

### Embedding Layer

`src/embedding/` contains:

- `factory.rs` for configured provider construction and vector-space identity
- `openai.rs` for OpenAI-compatible embedding endpoints
- `noop.rs` for text-only mode
- `resilient.rs` for availability tracking and graceful degradation
- `orchestrator.rs` for background task coordination

If the embedding endpoint is unavailable, LocalHold degrades to keyword/text-only search rather than failing closed.

### Store Layer

`src/store/` is split by concern:

- `schema.rs` for DDL, triggers, and migrations
- `crud.rs` for writes, entities, and audit-entry helpers
- `query.rs` for shared SQL column lists and filter construction
- `search.rs` for ANN, FTS5, and text fallback search
- `admin.rs` for eviction and scope reassignment
- `sqlite.rs` for the concrete store implementation
- `postgres.rs` for the opt-in PostgreSQL store implementation
- `vector/` for shared vector result types used by backend-specific vector indexes

## Data Model

The main persisted objects are:

- `memories` rows with content, tags, provenance, access policy, timestamps, importance, and supersession metadata
- `memory_embeddings` plus backend-specific vector indexing for vector search
- `memory_entities` for typed entity attachment and expansion
- `memory_fts` for SQLite FTS5 keyword search; PostgreSQL uses a `to_tsvector('simple', content)` index
- `memory_audit_log` for append-only write history
- `scope_registry` for tool-managed scope definitions, aliases, and matchers
- `memory_v2_metadata` for non-destructive v2 card metadata, scope keys,
  quality flags, migration markers, and principal provenance

See `src/types.rs` for the domain model, `src/store/schema.rs` for the SQLite schema, and `src/store/postgres.rs` for the PostgreSQL schema bootstrap.

## Search Model

Current code supports:

- semantic search via embeddings
- keyword search via SQLite FTS5 or PostgreSQL full-text search
- text fallback search
- hybrid search using Reciprocal Rank Fusion
- configurable result sorting and entity expansion

The ranking and search behavior in code should be treated as authoritative over older notes that may still exist elsewhere.

## Operational Model

- memories are persisted before background embedding work starts
- access control is enforced at read and write boundaries
- v2 authorization uses server-resolved principals rather than caller-provided labels
- stdio uses one trusted principal per server instance; shared multi-agent
  deployments need distinct trusted principals, typically via separate stdio
  instances or explicit trusted-proxy HTTP mode behind an authenticating proxy
- bearer-authenticated HTTP uses one fixed principal by default and ignores
  caller-supplied identity headers
- scope is retrieval/write context and is resolved from explicit scope values,
  registered aliases, or context matchers
- mutating memory writes commit audit rows transactionally with the associated
  mutation, required v2 metadata, and tombstone or supersession state; search
  impressions and other analytics remain best-effort operational metadata
- deletes retain minimal authorization tombstones so post-delete history can be
  authorized without retaining recallable memory content; missing tombstones
  fail closed and return no deleted-memory history
- the server can run over stdio or HTTP
- stdio and HTTP transports share the same server and engine layers

## Key Files

- `src/server/mod.rs`
- `src/server/params.rs`
- `src/engine.rs`
- `src/types.rs`
- `src/config.rs`
- `src/store/sqlite.rs`
- `src/store/postgres.rs`
- `src/store/schema.rs`
- `src/store/search.rs`
- `src/embedding/orchestrator.rs`

## Related Docs

- Example configuration: [../localhold.example.toml](../localhold.example.toml)
- V2 agent API: [agent-api-v2.md](agent-api-v2.md)
