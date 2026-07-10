# Contributor Agent Guidelines

These rules apply to automated coding agents working in LocalHold. Keep changes
small, reviewable, and grounded in the current repository.

## Working Rules

- Read the relevant code and tests before editing.
- Follow existing patterns before adding dependencies or abstractions.
- Keep behavior changes separate from unrelated cleanup.
- Do not revert changes you did not make.
- Never commit credentials, local configuration, databases, model files, build
  output, personal paths, or internal planning material.
- Treat `LOCALHOLD_*` values and memory content as potentially sensitive.

## Repository Map

- Core server and business logic: `src/`
- MCP handlers and wire types: `src/server/`
- Persistence: `src/store/`
- Embedding providers: `src/embedding/`
- Reranking: `src/reranker/`
- Integration tests: `tests/integration/`
- Benchmarks: `benches/`
- Maintained documentation: `docs/`

## Verification

Use the pinned tools from `mise.toml` and prefer `just` recipes:

- `just test` for the test suite
- `just clippy` for linting
- `just fmt-check` for formatting
- `just deny` for dependency policy
- `just check` for the complete local gate

Run targeted tests first for narrow changes, then broaden validation when a
change crosses transport, storage, authorization, embedding, or reranking
boundaries. Windows is validated by GitHub Actions and is currently preview
support. State any validation that could not be run.

## Documentation

Documentation in this repository must serve a current user, operator, or
contributor need. Do not add task journals, review transcripts, completed
implementation plans, session notes, or historical audits. Track unfinished
work in GitHub issues and preserve durable decisions in current architecture or
policy documentation.

## Definition Of Done

- Behavior and public contracts are covered by focused tests.
- Relevant documentation and examples match the implementation.
- Required checks pass, or the exact gap is reported.
- The diff contains no generated output, secrets, personal paths, or unrelated
  files.
