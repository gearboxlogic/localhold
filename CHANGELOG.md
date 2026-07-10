# Changelog

All notable changes to LocalHold will be documented in this file.

The project follows [Semantic Versioning](https://semver.org/). During the
`0.x` beta series, release notes will identify compatibility and data migration
requirements explicitly.

## [Unreleased]

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
