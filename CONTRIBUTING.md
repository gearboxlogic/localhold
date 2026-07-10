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

## Development Setup

```sh
./script/bootstrap.sh
just test
just check
```

The toolchain and development utilities are pinned by `mise.toml` and
`mise.lock`. Windows support is preview and is validated in GitHub Actions.

## Pull Requests

- Keep each change focused and explain user-visible behavior.
- Add regression tests for bugs and contract tests for public surfaces.
- Update maintained documentation and examples when behavior changes.
- Run `just check`, or state exactly which checks could not be run.
- Preserve compatibility unless the change is explicitly approved as breaking.

See [AGENTS.md](AGENTS.md) for additional guidance used by automated coding
agents.
