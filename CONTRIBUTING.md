# Contributing

LocalHold accepts contributions under the Apache License 2.0. By intentionally
submitting a contribution for inclusion, you agree that it may be distributed
under that license.

Participation is also governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before Starting

Use a GitHub issue to discuss substantial behavior, protocol, schema, or
dependency changes before implementation. Small bug fixes and documentation
corrections can go directly to a pull request.

Do not include credentials, memory databases, model files, generated build
output, personal paths, task journals, review transcripts, or internal planning
documents.

## Development Setup

First install the standard source-build requirements from
[Installation](docs/installation.md). To run the repository's complete local
check suite, also install [mise](https://mise.jdx.dev/); the bootstrap script
uses it to install tools including the pinned Rust toolchain, `just`, nextest,
cargo-deny, cargo-audit, cargo-machete, gitleaks, and the nightly rustfmt
component used by the formatting gate.

```sh
./script/bootstrap.sh
just test
just check
```

The development utilities are pinned by `mise.toml` and `mise.lock`. They are
required for full local CI parity, not for building or running LocalHold.

Additional tools are workflow-specific:

- ShellCheck is optional for manually linting changes to shell scripts.
- Docker and PostgreSQL client tools are required only for
  `just test-postgres-smoke`.
- NVIDIA/CUDA dependencies are required only when validating the CUDA reranker
  profile.

Windows support is preview and is validated in GitHub Actions.

## Pull Requests

- Keep each change focused and explain user-visible behavior.
- Add regression tests for bugs and contract tests for public surfaces.
- Update maintained documentation and examples when behavior changes.
- Run `just check`, or state exactly which checks could not be run.
- Preserve compatibility unless the change is explicitly approved as breaking.

See [AGENTS.md](AGENTS.md) for additional guidance used by automated coding
agents.
