#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
if [[ ${1:-} == --root ]]; then
    if (( $# < 2 )); then
        printf 'usage: check-maintainability-bootstrap.sh [--root PATH] [-- COMMAND...]\n' >&2
        exit 1
    fi
    repository_root=$2
    shift 2
fi
repository_root=$(cd -- "$repository_root" && pwd -P)

command=()
if (( $# > 0 )); then
    if [[ $1 != -- || $# == 1 ]]; then
        printf 'usage: check-maintainability-bootstrap.sh [--root PATH] [-- COMMAND...]\n' >&2
        exit 1
    fi
    shift
    command=("$@")
fi

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

reject_cargo_config() {
    local directory=$1
    local path
    for name in config.toml config; do
        path="$directory/$name"
        if [[ -e "$path" || -L "$path" ]]; then
            printf 'Cargo configuration is unsupported before maintainability checker compilation: %s\n' "$path" >&2
            exit 1
        fi
    done
}

directory=$repository_root
while :; do
    reject_cargo_config "$directory/.cargo"
    parent=$(dirname -- "$directory")
    [[ $parent == "$directory" ]] && break
    directory=$parent
done

cargo_home=${CARGO_HOME:-}
if [[ -z $cargo_home ]]; then
    if [[ -n ${HOME:-} ]]; then
        cargo_home="$HOME/.cargo"
    elif [[ -n ${USERPROFILE:-} ]]; then
        cargo_home="$USERPROFILE/.cargo"
    fi
elif [[ $cargo_home != /* && ! $cargo_home =~ ^[[:alpha:]]:[/\\] ]]; then
    cargo_home="$repository_root/$cargo_home"
fi
if [[ $cargo_home =~ ^[[:alpha:]]:[/\\] ]] && command -v cygpath >/dev/null 2>&1; then
    cargo_home=$(cygpath -u "$cargo_home")
fi
if [[ -n $cargo_home ]]; then
    reject_cargo_config "$cargo_home"
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

if (( ${#command[@]} > 0 )); then
    while IFS= read -r name; do
        uppercase=${name^^}
        case "$uppercase" in
            RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_BUILD_TARGET | CLIPPY_ARGS | RUSTC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTFLAGS | \
                CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | \
                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER)
                unset "$name"
                ;;
        esac
    done < <(compgen -e)
    exec "${command[@]}"
fi
