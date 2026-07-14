# Changelog

All notable changes to LocalHold will be documented in this file.

The project follows [Semantic Versioning](https://semver.org/). During the
`0.x` beta series, release notes will identify compatibility and data migration
requirements explicitly.

## [Unreleased]

- Added explicit reranker execution-provider policy with `auto`, `cpu`, and
  `cuda` modes, truthful compiled/selected/active provider reporting, and
  optional required-mode startup enforcement.
- Made CUDA-capable builds fall back visibly to a CPU session only in `auto`
  mode; an explicit `cuda` request never silently falls back.
- Updated the ONNX Runtime integration to `ort 2.0.0-rc.12` and the 1.24 ABI
  so CUDA deployments can use a version-matched runtime without compatibility
  warnings.

## [0.1.0-beta.3] - 2026-07-10

- Corrected MCP initialize metadata to report LocalHold's name and package
  version instead of the underlying SDK's identity.

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

[Unreleased]: https://github.com/gearboxlogic/localhold/compare/v0.1.0-beta.3...HEAD
[0.1.0-beta.3]: https://github.com/gearboxlogic/localhold/compare/v0.1.0-beta.2...v0.1.0-beta.3
[0.1.0-beta.2]: https://github.com/gearboxlogic/localhold/compare/v0.1.0-beta.1...v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/gearboxlogic/localhold/tree/v0.1.0-beta.1
