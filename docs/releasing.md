# Releasing

LocalHold releases are built from reviewed commits on `main`. Git tags trigger
the release workflow; maintainers do not upload locally built binaries.

## Prepare The Release

1. Update the package version in `Cargo.toml` and refresh `Cargo.lock`.
2. Move user-visible changes from `[Unreleased]` to a dated
   `[VERSION] - YYYY-MM-DD` section in `CHANGELOG.md`.
3. Update the tagged source-install example in
   [Installation](installation.md).
4. Run `python3 script/release.py validate vVERSION` and `just check`.
5. Open a release preparation pull request. Merge only after required CI is
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
4. extracts and smoke-tests both archives;
5. creates `SHA256SUMS`; and
6. publishes a GitHub prerelease when the version contains a prerelease suffix.

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

CUDA reranking remains source-build-only until LocalHold defines and validates a
portable ONNX Runtime, CUDA, cuDNN, driver, and dynamic-loader compatibility
matrix.
