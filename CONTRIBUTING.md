# Contributing

LocalHold accepts contributions under the Apache License 2.0. By intentionally
submitting a contribution for inclusion, you agree that it may be distributed
under that license.

Participation is also governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before Starting

Use a GitHub issue to discuss substantial behavior, protocol, schema, or
dependency changes before implementation. Small bug fixes and documentation
corrections can go directly to a pull request.

LocalHold is under a maintainability recovery feature freeze. New product
features and public API expansion are not accepted while the freeze is active.
Focused maintainability work, critical correctness/data-integrity/security
fixes, dependency security maintenance, and release-blocking platform work
remain in scope. Any exception must be approved and tracked before merge.

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
- Claude Code is optional for local adversarial review. When used, invoke it
  through `./script/claude-review.sh <opus|fable>` so MCP servers and other
  customizations stay disabled and per-review scratch data is cleaned up.
- Docker and PostgreSQL client tools are required only for
  `just test-postgres-smoke`.
- NVIDIA/CUDA dependencies are required only when validating the CUDA reranker
  profile.
- Python 3 is required only for release validation and archive packaging.

Windows support is preview and is validated in GitHub Actions.
macOS source builds are best-effort and are not release-gated; the complete
`just check` path requires a native Linux or Windows checkout because dependency
evidence is platform-specific. `just check-quality` runs the non-evidence
checks on macOS, but it does not satisfy the complete merge gate.

### Dependency unsafe exposure

`just maintainability` includes a fail-closed audit of the dependency graphs
and packaged sources selected for the native Linux or Windows target. The audit
uses Cargo's target/feature graph, verifies every cached crates.io archive
against `Cargo.lock`, resolves and vendors from an isolated Cargo home, then
verifies the vendored file set and file hashes before scanning. It tokenizes all
packaged Rust source and records native, prebuilt, generated-source, build
script, and proc-macro signals. It is deliberately conservative: a recorded
signal means code exists in the selected package distribution, not that the
code is compiled in every profile or that an unsafe boundary is unsound.

Every exposed package has an exact version-and-checksum classification under
`policy/dependency-unsafe/classifications/`. The Linux and Windows evidence is
separate. Matrix `profile` values identify review lanes; the packaged-source
scanner does not execute builds and Cargo dependency resolution is
profile-independent. After an approved dependency safety review, regenerate
only on the matching native operating system:

```sh
just dependency-unsafe-generate linux
# On a native Windows checkout:
just dependency-unsafe-generate windows
```

If Windows CI reports a missing or stale baseline, it uploads
`dependency-unsafe-windows-<commit>`. Review the dependency and policy change,
download the artifact, and copy its complete `actual-windows/` directory to
`policy/dependency-unsafe/baseline/windows/`. This is native Windows evidence
produced by CI; a Linux cross-target inventory cannot replace it.

Do not edit generated evidence to make the gate pass. New packages, features,
routes, checksums, or exposure signals require review before updating the
classification and baseline.

The Rust selectors in `rust-toolchain.toml`, `mise.toml`, and `mise.lock` are
exact patch pins because the evidence records exact Cargo and rustc builds.
Non-test audit-tool source or lockfile changes intentionally invalidate both
native baselines, even when a refactor appears output-neutral. This forces the
scanner implementation used for evidence to receive the same native review as
dependency changes.

The scanner invokes the exact absolute Cargo and rustc executables that built
it. It refuses Cargo configuration files in the isolated working directory's
physical ancestor chain. Source/registry override environment variables are
removed from Cargo subprocesses. Move or remove inherited Cargo configuration
before running the audit; source replacement is not a supported audit input.

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
- Greptile reviews a pull request only once, as its initial review. Do not
  re-invoke Greptile after subsequent pushes.
- CodeRabbit is a required merge gate. Wait for its review of the latest commit
  and address any actionable findings.
- Every push receives a fresh Codex review. Address actionable findings and
  wait for Codex approval of the latest commit. If review does not start,
  comment `@codex` on the pull request to request it.
- Reply to each review comment with the applied fix or the rationale for not
  changing it, then resolve the review thread.
- Merge only when required CI is green, the initial Greptile and latest-commit
  CodeRabbit reviews have no outstanding actionable findings, and Codex has
  approved the latest commit.
- Preserve compatibility unless the change is explicitly approved as breaking.

See [AGENTS.md](AGENTS.md) for additional guidance used by automated coding
agents. Release maintainers must also follow [Releasing](docs/releasing.md).
