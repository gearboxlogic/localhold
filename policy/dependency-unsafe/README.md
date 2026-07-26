# Dependency Source Exposure Policy

This directory contains the machine-readable policy and native evidence used by
`just dependency-unsafe`.

- `matrix.json` defines the native Linux and Windows audit lanes. A lane's
  `profile` is review metadata; Cargo dependency resolution itself is
  profile-independent. Features and development-edge selection determine the
  graph.
- `classifications/policy.json` defines the responsible owner and the complete
  trigger set shared by every classification.
- The remaining classification files are alphabetical buckets keyed by exact
  crates.io name, version, and checksum. Per-package rationales are descriptive
  source-exposure categories, not soundness approvals. Their exact entries are
  an explicit review checkpoint: generated exposure signals do not
  automatically classify or approve a package.
- `baseline/<platform>/` is generated evidence. Do not edit individual records
  or use a cross-target inventory as a native baseline.

The `mature-ffi` category is reserved for a substantive package-boundary review.
The initial conservative baseline does not assign it automatically.

When a native Windows check reports missing or stale evidence, CI uploads
`dependency-unsafe-windows-<commit>` containing the complete
`actual-windows/` directory. Review that artifact and the associated dependency
or scanner change, then copy the directory intact to `baseline/windows/`.
Regenerating or promoting evidence does not by itself approve a classification;
the policy diff and source signals still require review.

The audit first copies checksum-verified `.crate` archives and the matching
registry index into a temporary isolated Cargo home. Cargo graph and vendor
commands use the absolute Cargo executable that built the scanner and run from
a canonical working directory whose physical ancestor chain is checked before
every invocation. Cargo config files are refused, and source/registry override
environment variables are removed from Cargo subprocesses, so source
replacement cannot make the resolved graph differ from the scanned bytes.
Temporary registry and vendor data lives under the ignored
`.cache/dependency-unsafe/` directory, which is intentionally outside CI's
persisted Cargo/build cache paths.

The manifest binds evidence to the non-test audit-tool source and its lockfile.
That conservative coupling means a scanner implementation or dependency change
requires refreshing both native baselines; test-only edits do not.
