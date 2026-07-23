# Changelog

All notable changes to LocalHold will be documented in this file.

The project follows [Semantic Versioning](https://semver.org/). During the
`0.x` series, release notes identify compatibility and data migration
requirements explicitly.

## [Unreleased]

- Changed whole-store expiry cleanup to record the server-resolved principal in
  each tombstone and transactional per-memory delete audit row; cleanup still
  selects all expired memories without per-memory policy filtering.
  `MemoryAdmin::evict_expired` now requires that principal and audit draft.

## [0.2.0] - 2026-07-15

- Changed malformed typed `LOCALHOLD_*` environment overrides to fail startup
  and operator commands without echoing their values; `hold doctor` now reports
  configuration failure and skips dependent probes when effective
  configuration is unavailable.
- Changed PostgreSQL startup to validate the resolved managed schema before
  serving: automatic migration bootstraps or repairs compatible schemas,
  disabled migration rejects absent or partial schemas, and all modes reject
  incompatible or conflicting schemas.
- Changed PostgreSQL schema initialization and migration to run atomically
  under a serialized advisory lock, with a configurable bound on each lock wait
  and exact migration-ledger validation.
- Added `hold ui`, an interactive terminal browser with search, detail,
  metadata, history, edit, expiry, and delete workflows. Browsing remains
  read-only; write access is acquired only for confirmed mutations.
- Added optimistic concurrency for interactive memory edits and deletes across
  SQLite and PostgreSQL. A separate record revision detects metadata and
  lifecycle changes without making unchanged content appear freshly updated.
- Added supported WAL-safe SQLite backup and restore commands with validation,
  dry runs, cross-process coordination, retained recovery snapshots, and
  transactional rollback when replacement or post-write validation fails.
- Added `hold doctor` with stable text and JSON diagnostics for configuration,
  storage schemas, embedding profiles, vector consistency, reranker health,
  and repairable versus blocking conditions.
- Added `hold config init`, `hold config validate`, and `hold config paths` for
  safe operator setup, validation, and active-path discovery.
- Added model artifact operator commands and `hold embeddings status [--json]`
  with provider health, secret-free configured/stored profile identity, vector
  coverage, consistency diagnostics, stable states, and automation-friendly
  exit codes.
- Added explicit reranker execution-provider policy with `auto`, `cpu`, and
  `cuda` modes, truthful compiled/selected/active reporting, required-mode
  startup enforcement, and process-safe model cache downloads.
- Replaced the built-in reranker raw ONNX graph with a checksum-pinned fused
  FP32 artifact and added an opt-in fused FP16 profile for explicit CUDA use.
  FP16 is rejected with CPU or `auto` fallback because it trades ranking
  precision for lower weight storage and faster CUDA reranking.
- Expanded release packaging with a deterministic Linux x86_64 CUDA 12 archive
  alongside the existing Linux CPU and Windows preview archives, and hardened
  them with pinned runtime inputs, private library loading, manifests, notices,
  checksums, extracted-artifact smoke tests, and Ubuntu 22.04/glibc-floor
  enforcement.
- Added a protected real-GPU release gate that validates the exact CUDA archive
  for native dependency closure, clean-host startup, FP32/FP16 parity,
  performance, and resource limits before publication.
- Added the LocalHold brand identity and maintained design-system assets.
- Hardened schema diagnostics, PostgreSQL index checks, redaction boundaries,
  pgvector-backed PostgreSQL CI coverage, release smoke tests, and CUDA runtime
  error reporting; removed retired API artifacts and made runtime time testable
  without wall-clock sleeps.
- Compatibility: opening a SQLite v1 database upgrades it to schema v2, and
  restore upgrades a strictly validated v1 backup only on a private staging
  copy. Older binaries reject schema v2 databases. PostgreSQL startup adds the
  matching record-revision migration when automatic migration is enabled.

## [0.1.0-beta.3] - 2026-07-10

- Corrected MCP initialize metadata to report LocalHold package name and
  version instead of the underlying SDK identity.

## [0.1.0-beta.2] - 2026-07-10

- Restored CPU reranker builds on the Ubuntu 22.04 release baseline by pinning
  the ONNX Runtime integration to a glibc-compatible release.
- Added an Ubuntu 22.04 reranker-enabled source test to pull request CI.

## [0.1.0-beta.1] - 2026-07-10

- Established the curated LocalHold public source tree.
- Reset the public version stream to `0.1.0-beta.1`.
- Renamed the package to `localhold` and the executable to `hold`.
- Standardized runtime naming on `LocalHoldEngine`, `LocalHoldServer`,
  `LOCALHOLD_*` environment variables, and the `localhold_migrations` table;
  removed the legacy alternate config filename.
- Made text-only search the default until an OpenAI-compatible embedding
  endpoint is configured.
- Added a formal source installer with CPU and CUDA reranker profiles.
- Added persisted embedding vector-space identity and an explicit reindex
  transition for provider, model, endpoint, or dimension changes.
- Added reproducible Linux CPU and Windows preview release archives with
  extracted-artifact smoke tests, checksums, and automated GitHub prereleases.

[Unreleased]: https://github.com/gearboxlogic/localhold/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/gearboxlogic/localhold/compare/v0.1.0-beta.3...v0.2.0
[0.1.0-beta.3]: https://github.com/gearboxlogic/localhold/compare/v0.1.0-beta.2...v0.1.0-beta.3
[0.1.0-beta.2]: https://github.com/gearboxlogic/localhold/compare/v0.1.0-beta.1...v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/gearboxlogic/localhold/tree/v0.1.0-beta.1
