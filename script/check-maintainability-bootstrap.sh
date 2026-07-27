#!/usr/bin/env bash
set -euo pipefail

repository_root=${1:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"}
tool_root="$repository_root/tools/maintainability"
manifest="$tool_root/Cargo.toml"

if [[ ! -f "$manifest" ]]; then
    printf 'maintainability bootstrap manifest is missing: %s\n' "$manifest" >&2
    exit 1
fi

if [[ -e "$tool_root/build.rs" ]]; then
    printf 'maintainability checker build.rs is unsupported: %s\n' "$tool_root/build.rs" >&2
    exit 1
fi

build_setting=$(
    awk '
        /^\[package\][[:space:]]*(#.*)?$/ { in_package = 1; next }
        /^\[/ { in_package = 0 }
        in_package && /^[[:space:]]*build[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            sub(/[[:space:]]*#.*/, "", value)
            gsub(/[[:space:]]/, "", value)
            print value
        }
    ' "$manifest"
)

if [[ "$build_setting" != false ]]; then
    printf 'maintainability checker Cargo.toml must set [package] build = false\n' >&2
    exit 1
fi

printf 'maintainability bootstrap check passed\n'
