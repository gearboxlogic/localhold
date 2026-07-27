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
Opaque `[< ... >]` token-pasting input is rejected independently of the macro's
name or import path, including inside attribute input. Function-like macro and
attribute paths, plus derive macros, use closed reviewed sets; a new expansion
path requires an explicit safety-gate review before first use.
Reviewed macro definitions may not generate `use`, `mod`, or `extern crate`
bindings, because those declarations could redirect a trusted expansion path.
Macro invocations inside unsafe blocks and unsafe functions are also rejected
because changing the macro definition could change the reviewed operation
without changing the unsafe-context syntax; unsafe extern blocks are treated as
unsafe contexts too, as are unsafe trait and impl bodies.
`allow`, `expect`, and `warn` all count as exceptions when they weaken a
required deny-level lint. Each nested `cfg_attr` lint attribute is evaluated
independently so an unrelated weak lint level cannot be combined with a
safety-strengthening lint. Opaque macro-generated attributes, including
documentation attributes, `include!` code expansion (including imported
aliases), and `#[path]` source overrides are rejected so code cannot escape the
audited roots or hide an exception from the inventory. Explicit Cargo target
paths must remain under those roots. All enabled package build scripts are
rejected until the gate can audit their complete inputs and outputs; a physical
root `build.rs` is rejected even when disabled. The maintainability checker
itself sets `build = false`, and an independent bootstrap check runs before
Cargo to reject a physical checker `build.rs`, removal of that setting, and
repository, ancestor, or Cargo-home configuration. The bootstrap also removes
compiler, wrapper, linker, and runner override environment variables from the
Cargo process.
Runnable Rust doctests are likewise rejected, including rustdoc-only modifiers,
target-specific ignores, fences inside blockquotes or list items, and class-only
fences that remain Rust unless marked `custom`; `custom` takes precedence over
all other fence tokens. Closing fences must match the opening marker and meet
its delimiter length, so shorter Markdown examples embedded in non-Rust fences
remain documentation. Ordinary indented Markdown code blocks remain supported
because Rustdoc does not schedule them as doctests. Use maintained integration
tests for executable examples. Globally ignored and explicitly non-Rust
language blocks remain supported. The gate also runs Clippy with forced
`unsafe_code`, `unsafe_op_in_unsafe_fn`, and undocumented-block diagnostics.
Each emitted `unsafe_code` diagnostic must map to exactly one inventoried source
keyword; the other two diagnostics are always errors. Cargo dep-info for every
root target is checked and rejects recorded compiler inputs outside the audited
roots, including generated `include!` inputs. Compiler diagnostics from
library, binary, integration-test, benchmark, and example targets are audited
in their applicable normal and test configurations. The benchmark lane accepts
every audited target kind selected by Cargo, including benchmark-enabled
libraries, binaries, examples, and tests. Other audit lanes are scheduled from
locked Cargo metadata rather than conventional directory presence, so explicit
targets under other audited roots and test-enabled examples remain covered.

Every exception requires a narrow safety contract with a stable owner,
necessity, attempted safe alternatives, validity/lifetime/aliasing/ABI/thread
and target invariants, caller preconditions, safe wrapper boundary, focused
tests, dependency pins, invalidation and removal triggers, proof debt, and a
recovery issue. Each focused-test reference must resolve to an existing,
unconditional, non-ignored explicit test function that is the source of a
Cargo-scheduled integration-test target using the standard test harness without
opt-in target features. Its complete normalized test-function syntax is
fingerprinted in the contract, so weakening or replacing the test requires
explicit safety review. Operations and lint exceptions are counted separately.
Site locators plus site and enclosing-boundary syntax fingerprints, including
enclosing impl, trait, and extern headers plus named type/data and
associated-item boundaries that can contain const expressions, make additions,
moves, removals, operation mutations, and safe-wrapper mutations fail closed.
The gate also reserves a higher Cargo priority for required compiler and Clippy
lints so overlapping lint groups cannot override them, while unrelated lints
may use their own priorities. It sanitizes compiler override environment
variables and inherited build-target selection, and rejects `.cargo/config*`
files in the repository, its ancestors, or Cargo home that could redirect or
override compiler audits. Contract dependencies must use exactly their reviewed
routes: alternate direct
dependencies, aliases, workspace inheritance, build/dev/target declarations,
and root-feature forwarding are rejected. Locked versions, sources, checksums,
direct feature specifications, fully resolved feature sets, and incoming
dependency-graph routes remain pinned under the all-features graph. First-party
Rust must remain in the audited root package: root workspaces, package-level
external workspace inheritance, and local path dependencies are rejected,
except for the exact reviewed self dev-dependency used by integration tests.
The broader dependency-exposure gate still reviews target-specific effective
Cargo graphs.

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

### Source structure budgets

The same checked-in source audit enforces the structural baseline in
`policy/maintainability/structure.json`. Every Rust file under `src/`, `tests/`,
and `benches/` belongs to exactly one logical component. An unlisted file, a
deleted path left in the map, or an existing path reassigned to another
component fails closed.

The checker parses Rust syntax and classifies `#[cfg(test)]` and
`cfg(feature = "testing")` items as test lines. Files reachable only through
those modules, plus integration tests and benchmarks, are entirely test-only.
Explicit Cargo test and benchmark targets are also test-only even when their
audited path is under `src/`; explicit examples remain production roots.
This keeps extracting inline tests from pretending that production code was
removed. LF and CRLF checkouts produce the same physical-line count.
Production library and binary targets must remain under `src/`. Explicit or
conditional module path overrides and Rust-source `include!` calls are rejected
because they make source ownership and production reachability ambiguous.
No package feature may enable the test-only `testing` feature; it must remain
available only through an explicit test invocation.
Rust examples and explicitly declared Cargo targets outside the tracked roots
are also rejected while the initial path map is closed.

During the feature freeze:

- ordinary production files may not exceed 800 physical lines;
- ordinary test files may not exceed 1,000 physical lines;
- every existing larger file has a stable hotspot ID, verified baseline counts,
  and a successor set;
- hotspot physical and production ceilings, plus component production
  ceilings, must match the current lower count and can never increase;
- path changes require an append-only `path_evolutions` record. A `rename`
  preserves physical and production counts exactly, a `split` may not increase
  their aggregate, and a `test-extraction` must preserve production exactly
  while adding a test-only successor. Sources must be active in the pull
  request base, every measured path change must be covered exactly once, and
  existing or retired paths cannot be used to hide a merge, replay, or
  resurrection;
- production moved between logical components also requires an append-only
  `component_transfers` record tied to the path evolution. Its amount must
  equal the syntax-classified production lines in its destination paths.
  Source ceilings lose exactly that amount, destination ceilings gain at most
  that amount, and transfer cycles are rejected;
- a hotspot is marked resolved when its current successor falls under the
  applicable file limit, while its closed lineage and ratcheted evidence remain;
- component growth, unmapped file proliferation, ceiling inflation, hotspot
  reactivation, and silent policy resets are rejected.

The baseline remains fixed at the recovery commit recorded in the manifest.
The one closed pre-gate adjustment records the three lines added by the
reviewed SQLite unsafe-boundary isolation before structural enforcement
existed; it is immutable and does not reset the original evidence.

Inspect current or baseline classification without writing files:

```sh
cargo run --manifest-path tools/maintainability/Cargo.toml --locked -- structure-inventory
cargo run --manifest-path tools/maintainability/Cargo.toml --locked -- \
  structure-inventory b05f7a43345b39d40b456fb9ed46d479c4bf26e0
```

When a change legitimately reduces a component or hotspot, lower the matching
ceiling in the same pull request. Do not raise or copy a ceiling merely to make
the gate pass. CI compares the manifest with the pull request base revision so
ratchets cannot be reversed by editing policy.

For a rename, split, or test extraction, append a stable lowercase record ID,
the old `sources`, current `successors`, and nonempty issue, pull-request, and
rationale evidence. Use `rename` for either a path rename or an exact one-to-one
ownership reassignment. Never edit or reorder an existing record. Update the
affected component `paths`; if a hotspot source changed, replace its
`successors` with the complete direct successor set without changing the
canonical ceilings. A cross-component production successor additionally needs
a transfer record naming the source and destination component, exact
`production_lines`, transferred paths, and the new path-evolution ID. Test-only
cross-component paths carry lineage but no production transfer, so a
test-only ownership move must also rename its path to preserve cycle evidence.

The checker reads and classifies both the pull request base and working tree.
Changing only the JSON cannot manufacture a transfer or reset debt: ledger
entries, path ownership, file counts, component ceilings, and hotspot
successors must all reconcile with those two inventories.

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

The first-party expanded-source audit invokes the Cargo executable that built
the maintainability tool. It refuses Cargo configuration files in the working
directory's physical ancestor chain and in Cargo home, and removes inherited
Cargo aliases, compiler wrappers, and Rust flags that could weaken or redirect
its forced safety lints. Move or remove inherited Cargo configuration before
running the audit.

Rustc suppresses safety-lint diagnostics for tokens synthesized entirely by an
external procedural macro. The lexical gate therefore permits only reviewed
macro and attribute paths, rejects aliases and local modules that can
impersonate their package roots, and requires those roots to map to unrenamed
Cargo dependencies. Cargo.lock and dependency-unsafe review pin the
implementation behind each permitted path. The expanded-source audit is a
second control for every safety diagnostic rustc does emit; it is not the
identity boundary for procedural macros.

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
