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
  customizations stay disabled, sensitive runtime environment variables are
  not inherited, and per-review scratch data is cleaned up. Authenticate the
  CLI through its normal user configuration rather than environment tokens.
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

### First-party unsafe boundaries

LocalHold denies unsafe Rust by default. `just source-safety`, included in
`just maintainability`, checks executable unsafe syntax, mutable statics,
Rust 2024 unsafe attributes, unsafe global-assembly macros, and direct or
group-level exceptions to required safety lints under `src/`, `tests/`, and
`benches/`, plus `examples/` when present, against
`policy/maintainability/unsafe.json`. The gate parses Rust syntax rather than
searching text, so comments, strings, and unsafe function-pointer types are not
mistaken for executable operations. Macro-generated unsafe syntax, assembly,
mutable statics, and safety-lint exceptions are rejected because a definition
fingerprint cannot account for caller-supplied expansions. Standalone assembly
macros must use explicit qualified paths so every invocation is fingerprinted.
Macro invocations inside unsafe blocks and unsafe functions are also rejected
because changing the macro definition could change the reviewed operation
without changing the unsafe-context syntax.
`allow`, `expect`, and `warn` all count as exceptions when they weaken a
required deny-level lint. Opaque macro-generated attributes, `include!` code
expansion (including imported aliases), and `#[path]` source overrides are
rejected so code cannot escape the audited roots or hide an exception from the
inventory. Explicit Cargo target paths must remain under those roots. A root
`build.rs` is rejected until the gate can audit its complete module graph.

Every exception requires a narrow safety contract with a stable owner,
necessity, attempted safe alternatives, validity/lifetime/aliasing/ABI/thread
and target invariants, caller preconditions, safe wrapper boundary, focused
tests, dependency pins, invalidation and removal triggers, proof debt, and a
recovery issue. Operations and lint exceptions are counted separately. Site
locators plus site and enclosing-boundary syntax fingerprints make additions,
moves, removals, operation mutations, and safe-wrapper mutations fail closed.
The gate also reserves a higher Cargo priority for required compiler and Clippy
lints so groups cannot override them, and rejects repository `.cargo/config*`
files that could inject overriding compiler flags. Contract dependencies must
use exactly their reviewed routes: alternate direct dependencies, aliases,
workspace inheritance, build/dev/target declarations, and root-feature
forwarding are rejected. Locked versions, sources, checksums, and direct
feature specifications remain pinned. The broader dependency-exposure gate
still reviews target-specific effective Cargo graphs.

Inspect the parser's current site inventory with:

```sh
cargo run --manifest-path tools/maintainability/Cargo.toml --locked -- inventory
```

The command prints the pretty-formatted site inventory to standard output for
inspection, but writes no files. Do not copy inventory fingerprints into
policy merely to make the gate pass. First remove the unsafe code when a safe
design exists. If unsafe remains necessary, declare exactly one reviewed
operation for each executable site, keep each block to that operation, update
the complete contract and focused tests, and request an explicit safety review.

The current sqlite-vec registration contract is temporary proof debt:
sqlite-vec exposes an erased Rust function signature while its bundled C header
declares the typed SQLite initializer. The manifest records exactly what is and
is not proven. A typed or safe upstream registration API triggers removal of
the transmute.

### Dependency unsafe exposure

`just maintainability` includes a fail-closed audit of the dependency graphs
and packaged sources selected for the native Linux or Windows target. The audit
uses Cargo's target/feature graph, verifies every cached crates.io archive
against `Cargo.lock`, vendors from an isolated Cargo home, then resolves the
graph through Cargo's controlled directory source backed by those vendored
manifests. It verifies the vendored file set and file hashes before scanning the
same tree. It tokenizes all packaged Rust source and records native, prebuilt,
generated-source, build script, and proc-macro signals. It is deliberately
conservative: a recorded signal means code exists in the selected package
distribution, not that the code is compiled in every profile or that an unsafe
boundary is unsound.

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
removed from Cargo subprocesses, as are inherited Rust flags that could change
`cfg` resolution. Move or remove inherited Cargo configuration before running
the audit; only the scanner's internally generated verified-vendor replacement
is supported.

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
