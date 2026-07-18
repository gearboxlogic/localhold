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

Repository documentation must describe a current user, operator, contributor,
architecture, or policy need. Task journals, review transcripts, historical
audits, and completed implementation plans belong in issues or the private
development archive, not this repository.

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
- Python 3 is required only for release validation and archive packaging.

Windows support is preview and is validated in GitHub Actions.

## Pull Requests

- Keep each change focused and explain user-visible behavior.
- Treat the pull request body as the authoritative association with GitHub
  issues. Use `Closes #N` only when merging the pull request fully satisfies
  that issue's acceptance criteria; use `Refs #N` for partial work,
  investigation, or prerequisites.
- Prefer one primary issue per pull request. Reference multiple issues only
  when the same inseparable change satisfies them. Link the focused child issue
  rather than closing a roadmap parent through one child change.
- Small bug fixes and documentation corrections may use `Not required` with a
  short reason instead of creating a tracking issue. Branch names and commit
  messages may include issue numbers, but they do not replace the pull request
  body association.
- After merge, add any acceptance evidence that is not visible from the pull
  request to the issue. For work completed outside a pull request, such as a
  release or operational rehearsal, comment with the workflow, artifact, and
  validation links and close the issue manually only after every acceptance
  criterion passes.
- Add regression tests for bugs and contract tests for public surfaces.
- Update maintained documentation and examples when behavior changes.
- Run `just check`, or state exactly which checks could not be run.
- Every push receives a fresh automated cloud review. Address actionable
  findings and wait for approval of the latest commit. If review does not
  start, comment `@codex` on the pull request to request it.
- Reply to each review comment with the applied fix or the rationale for not
  changing it, then resolve the review thread.
- Merge only when required CI is green and the cloud bot has approved the
  latest commit; neither condition is sufficient alone.
- Preserve compatibility unless the change is explicitly approved as breaking.

See [AGENTS.md](AGENTS.md) for additional guidance used by automated coding
agents. Release maintainers must also follow [Releasing](docs/releasing.md).
