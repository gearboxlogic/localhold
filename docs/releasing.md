# Releasing

LocalHold releases are built from reviewed commits on `main`. Git tags trigger
the release workflow; maintainers do not upload locally built binaries.

## Prepare The Release

1. Update the package version in `Cargo.toml` and refresh `Cargo.lock`.
2. Add deterministic SQLite and PostgreSQL fixture builders for the release to
   `tests/fixtures/database-upgrades/manifest.json`, including source provenance
   and effective SHA-256 checksums. In the same reviewed change, add the tag to
   `PUBLISHED_DATABASE_RELEASES` in `script/database_fixtures.py`. The trusted
   inventory and manifest must contain exactly the same tags; missing or extra
   releases fail `script/release.py validate`.
3. Move user-visible changes from `[Unreleased]` to a dated
   `[VERSION] - YYYY-MM-DD` section in `CHANGELOG.md`.
4. Update the tagged source-install example in
   [Installation](installation.md).
5. Run `python3 script/release.py validate vVERSION` and `just check`.
6. Open a release preparation pull request. Merge only after required CI is
   green and the cloud bot approves the latest head commit, following
   [the contributor workflow](../CONTRIBUTING.md).

## Publish

From an updated, clean `main`, create and push an annotated tag:

```sh
git tag -a vVERSION -m "LocalHold vVERSION"
git push origin vVERSION
```

The tag must point to a commit contained in `main`. The release workflow then:

1. validates the tag against Cargo metadata, the changelog, and installation
   documentation;
2. runs the release-source tests;
3. builds Linux x86_64 CPU and Windows x86_64 preview archives;
4. builds the checksum-pinned Linux x86_64 CUDA 12 archive from the exact tag
   on Ubuntu 22.04 and rejects binaries above the documented glibc 2.35 floor;
5. sends that exact archive to the protected NVIDIA runner and validates its
   native dependency closure, private loaded-library paths,
   clean-environment startup, FP32/FP16 parity, performance, and resources;
6. extracts and smoke-tests all archives;
7. creates `SHA256SUMS`; and
8. publishes a GitHub prerelease when the version contains a prerelease suffix.

After a successful release workflow, `Published Release Smoke` runs without a
source checkout. It downloads the public assets on clean Linux and Windows
runners, verifies their checksums, and exercises the extracted binaries. Rerun
it for an existing release with:

```sh
gh workflow run release-smoke.yml -f tag=vVERSION
```

Do not move or reuse a published tag. If the workflow fails before publication,
fix the problem through a reviewed pull request and create the next prerelease
version. If publication succeeds only partially, preserve the failed workflow
logs and remove the incomplete GitHub release before retrying the same tag job.

CUDA publication requires a successful protected-runner execution of the
[real-GPU reranker release gate](gpu-release-gate.md) for the exact protected
tag commit. The gate is a required job in the release workflow and produces the
archive consumed by the publish job; there is no manual copy, GPU-runner
rebuild, or separately approved artifact. Portable compilation on the hosted
Ubuntu baseline is necessary but is not sufficient release evidence—the same
archive must pass the protected real-GPU gate. Keep the FP32 and FP16 gate JSON
artifacts with the release run; a missing GPU,
unavailable RSS/VRAM metric, provider fallback, parity violation, or resource
threshold violation blocks publication.
