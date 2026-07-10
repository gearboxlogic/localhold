# LocalHold

[![CI](https://github.com/gearboxlogic/localhold/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/gearboxlogic/localhold/actions/workflows/ci.yml)
[![Dependency freshness](https://github.com/gearboxlogic/localhold/actions/workflows/outdated.yml/badge.svg?branch=main)](https://github.com/gearboxlogic/localhold/actions/workflows/outdated.yml)

**Searchable context that stays yours.**

LocalHold is local context infrastructure for AI agents. It runs as a standalone
[Model Context Protocol](https://modelcontextprotocol.io/) server and keeps
durable, searchable memory independent of any one agent or model provider.

LocalHold is in early beta. Linux CPU is the primary supported environment.
Windows, PostgreSQL, and CUDA reranking are preview surfaces.

## What It Provides

- SQLite storage by default, with optional PostgreSQL and `pgvector`
- MCP over stdio or streamable HTTP
- keyword, semantic, hybrid, and text fallback search
- OpenAI-compatible embedding endpoints, including local and cloud providers
- optional ONNX cross-encoder reranking on CPU or CUDA
- scoped memories, access policies, audit history, and maintenance tools

Storage is local by default. When an external embedding provider is enabled,
memory content and search queries are sent to the configured endpoint. LocalHold
does not start or manage model servers.

## Build From Source

Prerequisites:

- Git
- [mise](https://mise.jdx.dev/)
- a C compiler

```sh
git clone https://github.com/gearboxlogic/localhold.git
cd localhold
./script/bootstrap.sh
just build-release
./target/release/hold
```

The release build includes CPU reranker support, but reranking remains disabled
until configured. Standard binary installers are planned but are not available
in this beta snapshot.

## Configuration

LocalHold reads `./recall.toml` first, then
`~/.config/localhold/recall.toml`. Runtime overrides use `RECALL_*` environment
variables. See [recall.example.toml](recall.example.toml) for the complete
configuration surface.

The default provider is `noop`, which keeps search local and text-only. To use
semantic search, configure an OpenAI-compatible `/v1` endpoint:

```toml
[embedding]
provider = "openai_compatible"
dimensions = 768

[embedding.openai_compatible]
base_url = "http://127.0.0.1:8000/v1"
model = "nomic-embed-text"
# api_key = "..."
```

The endpoint must implement `GET /v1/models` and `POST /v1/embeddings`.
vLLM, llama.cpp, Ollama, and hosted services can be used when they expose this
contract. Ollama is one optional implementation; it is not required or assumed.

Embedding dimensions must match both the selected model and stored vectors.
Changing models or dimensions requires re-embedding existing memories.

### Reranking

Enable the built-in CPU reranker with:

```toml
[search.reranker]
enabled = true
```

The pinned model and tokenizer are downloaded on first use. For offline
deployments, set `model_path` and provide the model files in advance.

CUDA reranking is a preview build surface:

```sh
just build-release-reranker-cuda
```

It requires a compatible NVIDIA driver and ONNX Runtime/CUDA libraries. CUDA
does not affect embedding placement; embedding compute happens at the selected
OpenAI-compatible endpoint.

## MCP Client Setup

Build the binary, then configure an MCP client to launch `hold` over stdio:

```json
{
  "mcpServers": {
    "localhold": {
      "command": "/absolute/path/to/localhold/target/release/hold"
    }
  }
}
```

For clients with a command-based setup:

```sh
claude mcp add --scope user localhold /absolute/path/to/localhold/target/release/hold
```

The stdio server uses the configured `server.principal` as the trusted identity
for that process. Run separate instances when clients require separate
principals.

### HTTP Transport

```toml
[server]
transport = "http"
host = "127.0.0.1"
port = 8080
path = "/mcp"
http_auth_token = "replace-with-a-secret"
http_allowed_hosts = ["localhost", "127.0.0.1", "::1"]
```

The MCP endpoint is `http://127.0.0.1:8080/mcp`. HTTP requests never inherit
the stdio principal. Without `http_auth_token`, requests are anonymous and the
default policy allows public reads but denies writes.

The bearer token protects the endpoint but does not establish independently
verified user identity. A deployment that accepts caller-specific principal
headers must place LocalHold behind a trusted identity proxy and restrict direct
access to the server.

## Storage

SQLite is the default backend and stores data under
`~/.local/share/localhold/recall.db`. Back up the database and its WAL files as
a consistent set while the service is stopped or by using SQLite's backup
facilities.

PostgreSQL is opt-in:

```toml
[database]
backend = "postgres"

[database.postgres]
url = "postgres://localhold:password@localhost:5432/localhold"
```

To migrate an existing SQLite database into an empty PostgreSQL database:

```sh
hold migrate sqlite-to-postgres \
  --sqlite ~/.local/share/localhold/recall.db \
  --embedding-dimensions 768 \
  --dry-run
```

Review the dry run, then repeat with `--yes`. The destination must not already
contain user data.

## MCP Tools

The everyday API consists of `brief`, `recall`, `read`, `read_many`,
`remember`, `remember_many`, `handoff`, `revise`, and `forget`. Maintenance and
migration operations use explicit `admin_*` tools.

See [docs/agent-api-v2.md](docs/agent-api-v2.md) for tool semantics and
[docs/architecture.md](docs/architecture.md) for the current system design.

## Development

```sh
./script/bootstrap.sh
just test
just check
```

`just check` runs formatting, clippy, dependency policy, and tests. Linux and
Windows checks also run in GitHub Actions. Windows support is preview and is
validated by CI rather than a machine-specific local clone.

Repository documentation must describe a current user, operator, contributor,
architecture, or policy need. Task journals, review transcripts, historical
audits, and completed implementation plans belong in issues or the private
development archive, not this repository.

See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request and
[SECURITY.md](SECURITY.md) for vulnerability reporting.

## License

Copyright 2026 Gearbox Logic LLC.

Licensed under the [Apache License 2.0](LICENSE). The license does not grant
rights to LocalHold or Gearbox Logic trademarks; see [TRADEMARKS.md](TRADEMARKS.md).
Third-party components and models retain their respective licenses.
