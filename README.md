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

## Install A Release

GitHub prereleases provide a Linux x86_64 CPU archive and a Windows x86_64
preview archive. Each archive contains `hold`, the example configuration,
current documentation, and license notices. Verify the downloaded archive with
the accompanying `SHA256SUMS` file before extracting it. See
[Installation](docs/installation.md) for the archive layout and commands.

CUDA reranking remains a source-build preview and is not included in the
binary archives.

## Build From Source

Required for the standard CPU source install:

- Git
- Rust 1.97 with Cargo (the checked-in toolchain file pins this version)
- C and C++ compilers, CMake, and Make or Ninja
- on Linux, `pkg-config` and OpenSSL development headers
- network access to download Rust crates and the pinned ONNX Runtime artifact

For example, install the native packages with:

```sh
# Fedora
sudo dnf install gcc gcc-c++ cmake make pkgconf-pkg-config openssl-devel

# Debian/Ubuntu
sudo apt install build-essential cmake pkg-config libssl-dev
```

These are build requirements, not runtime service dependencies. `mise`, `just`,
ShellCheck, nextest, cargo-deny, and gitleaks are contributor/CI tools and are
not required to install or run `hold`. CUDA, PostgreSQL/pgvector, and an
OpenAI-compatible embedding endpoint are needed only when their corresponding
optional features are used.

```sh
git clone https://github.com/gearboxlogic/localhold.git
cd localhold
./script/install.sh
export PATH="$HOME/.local/bin:$PATH"
hold
```

The standard install includes CPU reranker support, but reranking remains
disabled until configured. See [Installation](docs/installation.md) for custom
prefixes, macOS and Windows prerequisites, and the CUDA preview profile.

## Configuration

LocalHold reads `~/.config/localhold/localhold.toml` on Linux and the
equivalent platform user-config directory elsewhere. It never loads config
implicitly from the current working directory. Runtime overrides use
`LOCALHOLD_*` environment variables. See
[localhold.example.toml](localhold.example.toml) for the complete configuration
surface.

Use `hold config paths` to show the canonical and active paths, `hold config
init` to create a minimal starter without replacing an existing file, and
`hold config validate` to validate the effective file and environment settings
without opening storage, contacting model providers, or starting the server.
Add `--json` to any of these commands for the versioned machine-readable
report.

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

The endpoint must implement `POST /embeddings` beneath the configured base URL.
The default startup check also requires `GET /models`; set
`health_check = "disabled"` for providers that omit that route. vLLM,
llama.cpp, Ollama, and hosted services can be used when they expose this
contract.
See [Embedding Providers](docs/embedding-providers.md) for cloud authentication,
dimensions, health checks, and transport security.

LocalHold records the endpoint, model, and dimensions that produced stored
vectors and refuses to mix a different vector space. To change any of them,
preserve a backup and run `hold embeddings reindex --yes`; memory content and
metadata are retained while vectors are rebuilt after the next start.

Inspect the configured and stored profiles, provider health, and rebuild
progress without starting the MCP server:

```sh
hold embeddings status
hold embeddings status --json
```

The report includes vector coverage and claim counts but never API keys or
memory content. See [Operations](docs/operations.md#embedding-status) for its
states and exit codes.

### Reranking

Enable the built-in CPU reranker with:

```toml
[search.reranker]
enabled = true
execution_provider = "cpu"
precision = "fp32"
```

The pinned, fused FP32 model and tokenizer are downloaded on first use. FP32
is also the default for CUDA and `auto`, so a CUDA failure can safely fall back
to CPU. For offline deployments, set `model_path` and provide the model files
in advance.

CUDA reranking is a preview build surface:

```sh
just build-release-reranker-cuda
```

The CUDA-capable binary supports `auto`, `cpu`, and `cuda`. `auto` prefers CUDA
after successful model inference and falls back visibly to CPU; `cpu` never
registers CUDA; explicit `cuda` never falls back to a CPU session. ONNX Runtime
may still place individual graph nodes on CPU. Set `required = true` when
startup must fail unless reranking is active. CUDA requires a compatible NVIDIA
driver and ONNX Runtime/CUDA libraries. It does not affect embedding placement;
embedding compute happens at the selected OpenAI-compatible endpoint.

CUDA users may opt into the smaller, faster fused FP16 artifact:

```toml
[search.reranker]
enabled = true
execution_provider = "cuda"
precision = "fp16"
```

FP16 is CUDA-only and can change the order of closely scored results. Treat it
as a latency/VRAM optimization, validate ranking quality on your own corpus,
and see [Operations](docs/operations.md#reranker-model-precision) for the
current evidence and limitations.

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
http_principal_mode = "fixed"
http_principal = "http"
http_allowed_hosts = ["localhost", "127.0.0.1", "::1"]
```

The MCP endpoint is `http://127.0.0.1:8080/mcp`. HTTP requests never inherit
the stdio principal. Without `http_auth_token`, requests are anonymous and the
default policy allows public reads but denies writes.

The default `fixed` mode assigns every valid bearer token the configured
`http_principal`; caller-supplied identity headers are ignored. For distinct
caller identities, select `trusted_proxy` mode only behind an authenticating
proxy that overwrites `x-localhold-principal` and prevents direct access to
LocalHold.

## Storage

SQLite is the default backend and stores data under
`~/.local/share/localhold/localhold.db`. Back up the database and its WAL files as
a consistent set while the service is stopped or by using SQLite's backup
facilities.

PostgreSQL is opt-in:

```toml
[database]
backend = "postgres"

[database.postgres]
url = "postgres://localhold:password@localhost:5432/localhold"
```

Check installation and runtime readiness without creating or migrating storage
or downloading models:

```sh
hold doctor
hold doctor --json
```

See [Operations](docs/operations.md) for diagnostic exit codes and the explicit
reranker download opt-in.

To migrate an existing SQLite database into an empty PostgreSQL database:

```sh
hold migrate sqlite-to-postgres \
  --sqlite ~/.local/share/localhold/localhold.db \
  --embedding-dimensions 768 \
  --dry-run
```

Review the dry run, then repeat with `--yes`. The destination must not already
contain user data.

## MCP Tools

The everyday API consists of `brief`, `recall`, `read`, `read_many`,
`remember`, `remember_many`, `handoff`, `revise`, and `forget`. Maintenance and
migration operations use explicit `admin_*` tools. Those privileged routes are
removed from discovery and dispatch by default; operators must set
`server.admin_tools_enabled = true` for a dedicated maintenance instance.

See [docs/agent-api.md](docs/agent-api.md) for tool semantics and
[docs/architecture.md](docs/architecture.md) for the current system design.
Operators should also read [Operations](docs/operations.md) and the
[Compatibility Policy](docs/compatibility.md).

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
