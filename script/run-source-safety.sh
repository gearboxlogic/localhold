#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly repository_root
cd -- "$repository_root"

readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}
readonly cargo_fmt_command=${LOCALHOLD_MAINTAINABILITY_CARGO_FMT:?maintainability bootstrap did not provide an absolute Cargo fmt command}
readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}
if [[ $cargo_command != /* && ! $cargo_command =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability bootstrap Cargo command must be absolute\n' >&2
    exit 1
fi
if [[ $cargo_fmt_command != /* && ! $cargo_fmt_command =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability bootstrap Cargo fmt command must be absolute\n' >&2
    exit 1
fi
if [[ $git_command != /* && ! $git_command =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability bootstrap Git command must be absolute\n' >&2
    exit 1
fi

"$cargo_command" fetch --manifest-path tools/maintainability/Cargo.toml --locked
"$cargo_fmt_command" --manifest-path tools/maintainability/Cargo.toml -- --check
"$cargo_command" test --manifest-path tools/maintainability/Cargo.toml --locked
"$cargo_command" clippy --manifest-path tools/maintainability/Cargo.toml --all-targets --locked -- -D warnings
"$cargo_command" run --manifest-path tools/maintainability/Cargo.toml --locked -- check
