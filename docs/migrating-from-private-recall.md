# Migrating From Gizmo Recall

This guide is for existing private Gizmo Recall installations moving to the
initial LocalHold beta. It is not a general downgrade path from the private
`2.x` package version to arbitrary public releases; the public version stream
was intentionally reset for the new repository.

## Before Migrating

1. Stop every process using the existing database.
2. Back up the SQLite database set or PostgreSQL database as described in
   [Operations](operations.md).
3. Preserve the old binary, config, and data until LocalHold has been verified.
4. Record the embedding provider, model, and dimensions used by the existing
   store.

## Name And Path Changes

| Private installation | LocalHold |
| --- | --- |
| Repository/package `gizmo-recall` | `localhold` |
| Executable `gizmo-recall` | `hold` |
| `~/.config/gizmo-recall/recall.toml` | `~/.config/localhold/localhold.toml` |
| `~/.local/share/gizmo-recall/recall.db` | `~/.local/share/localhold/localhold.db` |
| `~/.cache/gizmo-recall/models` | `~/.cache/localhold/models` |
| `x-recall-principal` | `x-localhold-principal` |

Documented `RECALL_*` environment variables remain stable during this
migration. Remove private promotion-launcher references and configure MCP
clients to invoke the `hold` executable directly.

## SQLite Migration

With all old processes stopped:

```sh
mkdir -p ~/.config/localhold ~/.local/share/localhold ~/.cache/localhold
cp ~/.config/gizmo-recall/recall.toml ~/.config/localhold/localhold.toml
cp ~/.local/share/gizmo-recall/recall.db ~/.local/share/localhold/localhold.db
cp ~/.local/share/gizmo-recall/recall.db-wal ~/.local/share/localhold/localhold.db-wal 2>/dev/null || true
cp ~/.local/share/gizmo-recall/recall.db-shm ~/.local/share/localhold/localhold.db-shm 2>/dev/null || true
```

Update paths inside `localhold.toml`, including the database and reranker cache
paths. Keep the existing embedding dimensions and provider settings for the
first startup. Then start `hold` and verify representative reads and text
searches before running bulk maintenance or re-embedding.

LocalHold temporarily recognizes `~/.config/localhold/recall.toml` when the
canonical file is absent, but moving directly to `localhold.toml` avoids an
additional migration later. Files in the current working directory are never
loaded implicitly.

## PostgreSQL Migration

The database itself does not move merely because the product name changed.
Copy the active PostgreSQL URL into the new config or `RECALL_POSTGRES_URL`,
start one LocalHold instance, and verify schema startup before allowing other
clients to connect. Do not run the SQLite-to-PostgreSQL migration against a
database that already contains user data.

## HTTP Identity Change

LocalHold defaults to one fixed identity for all callers possessing the HTTP
bearer token. Caller-provided principal headers are ignored in that mode. A
deployment that needs per-caller identities must explicitly select
`http_principal_mode = "trusted_proxy"` and satisfy the proxy isolation
requirements in [Operations](operations.md).

## Verification

- `hold` starts without reading a repository-local config file.
- MCP clients launch `hold`, not the old private launcher.
- representative public, restricted, and redacted memories retain access
  behavior;
- text search works before any provider change;
- semantic search uses the expected model and dimensions; and
- the old installation remains stopped until the migration is accepted.
