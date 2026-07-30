#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_directory=${script_path%/*}
[[ $script_directory != "$script_path" ]] || script_directory=.
repository_root=$(cd -- "$script_directory/.." && pwd -P)
cd -- "$repository_root"

readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}
if [[ $cargo_command != /* && ! $cargo_command =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability bootstrap Cargo command must be absolute\n' >&2
    exit 1
fi

"$cargo_command" fetch --manifest-path tools/maintainability/Cargo.toml --locked
"$cargo_command" fmt --manifest-path tools/maintainability/Cargo.toml -- --check
"$cargo_command" test --manifest-path tools/maintainability/Cargo.toml --locked
"$cargo_command" clippy --manifest-path tools/maintainability/Cargo.toml --all-targets --locked -- -D warnings
"$cargo_command" run --manifest-path tools/maintainability/Cargo.toml --locked -- check
