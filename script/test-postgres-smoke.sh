#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

say() { printf "\n==> %s\n" "$*"; }
die() { printf "\nERROR: %s\n" "$*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"; }

image="${LOCALHOLD_POSTGRES_IMAGE:-pgvector/pgvector:pg16}"
container="${LOCALHOLD_POSTGRES_CONTAINER:-localhold-postgres-smoke}"
port="${LOCALHOLD_POSTGRES_PORT:-55432}"
container_cli="${LOCALHOLD_CONTAINER_CLI:-docker}"
url="postgres://localhold:localhold@localhost:${port}/localhold"

cleanup() {
  "$container_cli" rm -f "$container" >/dev/null 2>&1 || true
}

main() {
  need_cmd "$container_cli"
  need_cmd cargo

  if "$container_cli" container inspect "$container" >/dev/null 2>&1; then
    die "PostgreSQL smoke container already exists: $container"
  fi

  trap cleanup EXIT
  say "Starting PostgreSQL pgvector smoke container"
  "$container_cli" run \
    --detach \
    --name "$container" \
    --publish "${port}:5432" \
    --env POSTGRES_USER=localhold \
    --env POSTGRES_PASSWORD=localhold \
    --env POSTGRES_DB=localhold \
    "$image" >/dev/null

  say "Waiting for PostgreSQL TCP readiness"
  ready=0
  for _ in {1..60}; do
    if "$container_cli" exec -e PGPASSWORD=localhold "$container" \
      psql -h 127.0.0.1 -U localhold -d localhold -v ON_ERROR_STOP=1 -c "SELECT 1" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  [[ "$ready" == "1" ]] || die "PostgreSQL did not become ready"
  export LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1

  say "Running ignored PostgreSQL bootstrap smoke test"
  LOCALHOLD_POSTGRES_URL="$url" cargo test -p localhold store::postgres::tests:: --locked -- --ignored --test-threads=1

  say "Running ignored PostgreSQL migration smoke test"
  LOCALHOLD_POSTGRES_URL="$url" cargo test -p localhold store::migration::tests:: --locked -- --ignored --test-threads=1

  say "Resetting PostgreSQL schema before binary smoke tests"
  "$container_cli" exec -e PGPASSWORD=localhold "$container" \
    psql -h 127.0.0.1 -U localhold -d localhold -v ON_ERROR_STOP=1 \
    -c "DROP TABLE IF EXISTS memory_audit_log, memory_tombstones, memory_metadata, memory_v2_metadata, memory_entities, memory_embeddings, embedding_profile, memories, scope_registry, localhold_migrations CASCADE" >/dev/null

  say "Running ignored PostgreSQL binary startup smoke test"
  LOCALHOLD_POSTGRES_URL="$url" cargo test -p localhold --test integration binary_smoke::binary_starts_with_postgres_backend --locked -- --ignored --test-threads=1

  say "Running ignored PostgreSQL disabled-migration startup smoke test"
  LOCALHOLD_POSTGRES_URL="$url" cargo test -p localhold --test integration binary_smoke::binary_rejects_empty_postgres_when_auto_migrate_is_disabled --locked -- --ignored --test-threads=1

  say "Running ignored PostgreSQL binary migration smoke test"
  LOCALHOLD_POSTGRES_URL="$url" cargo test -p localhold --test integration binary_smoke::binary_migrates_sqlite_to_postgres --locked -- --ignored --test-threads=1

  say "Running ignored PostgreSQL embedding status smoke test"
  LOCALHOLD_POSTGRES_URL="$url" cargo test -p localhold --test integration binary_smoke::embeddings_status_reports_postgres_profile_and_progress --locked -- --ignored --test-threads=1
}

main "$@"
