#!/usr/bin/env bash
set -euo pipefail

implementation_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
audit_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}
if [[ $audit_root != /* && ! $audit_root =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability audit root must be absolute\n' >&2
    exit 1
fi
audit_root=$(cd -- "$audit_root" && pwd -P)
readonly implementation_root audit_root
cd -- "$audit_root"

readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}
readonly cargo_clippy_command=${LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY:?maintainability bootstrap did not provide an absolute Cargo Clippy command}
readonly cargo_fmt_command=${LOCALHOLD_MAINTAINABILITY_CARGO_FMT:?maintainability bootstrap did not provide an absolute Cargo fmt command}
readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}
if [[ $cargo_command != /* && ! $cargo_command =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability bootstrap Cargo command must be absolute\n' >&2
    exit 1
fi
if [[ $cargo_clippy_command != /* && ! $cargo_clippy_command =~ ^[[:alpha:]]:[/\\] ]]; then
    printf 'maintainability bootstrap Cargo Clippy command must be absolute\n' >&2
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

maintainability_manifest="$implementation_root/tools/maintainability/Cargo.toml"
readonly maintainability_manifest
"$cargo_command" fetch --manifest-path "$maintainability_manifest" --locked
"$cargo_fmt_command" --manifest-path "$maintainability_manifest" -- --check
"$cargo_command" test --manifest-path "$maintainability_manifest" --locked
"$cargo_clippy_command" clippy --manifest-path "$maintainability_manifest" --all-targets --locked -- -D warnings
"$cargo_command" run --manifest-path "$maintainability_manifest" --locked -- check
