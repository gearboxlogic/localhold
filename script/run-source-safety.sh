#!/usr/bin/env bash
set -euo pipefail

require_absolute() {
    local description=$1 value=$2
    if [[ $value != /* && ! $value =~ ^[[:alpha:]]:[/\\] ]]; then
        printf '%s must be absolute\n' "$description" >&2
        exit 1
    fi
}

implementation_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
audit_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}
if [[ $audit_root != /* && ! $audit_root =~ ^[[:alpha:]]:[/\\] ]]; then
    printf '%s must be absolute\n' 'maintainability audit root' >&2
    exit 1
fi
audit_root=$(cd -- "$audit_root" && pwd -P)
readonly implementation_root audit_root
cd -- "$audit_root"

readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}
readonly cargo_clippy_command=${LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY:?maintainability bootstrap did not provide an absolute Cargo Clippy command}
readonly cargo_fmt_command=${LOCALHOLD_MAINTAINABILITY_CARGO_FMT:?maintainability bootstrap did not provide an absolute Cargo fmt command}
readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}
require_absolute 'maintainability bootstrap Cargo command' "$cargo_command"
require_absolute 'maintainability bootstrap Cargo Clippy command' "$cargo_clippy_command"
require_absolute 'maintainability bootstrap Cargo fmt command' "$cargo_fmt_command"
require_absolute 'maintainability bootstrap Git command' "$git_command"

maintainability_manifest="$implementation_root/tools/maintainability/Cargo.toml"
readonly maintainability_manifest
"$cargo_command" fetch --manifest-path "$maintainability_manifest" --locked
"$cargo_fmt_command" --manifest-path "$maintainability_manifest" -- --check
"$cargo_command" test --manifest-path "$maintainability_manifest" --locked
"$cargo_clippy_command" clippy --manifest-path "$maintainability_manifest" --all-targets --locked -- -D warnings
"$cargo_command" run --manifest-path "$maintainability_manifest" --locked -- check
