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
lockfile="$tool_root/Cargo.lock"
readonly reviewed_manifest_sha256=cca207767614bd2c1d46bc06092b69e90157aeb450797fcc7cad4e1ed67c89b9
readonly reviewed_lockfile_sha256=825c6448351761aa5c4c6e1ce6b3696c927c4f46c5d43642846380d24f10467c

if [[ ! -f "$manifest" ]]; then
    printf 'maintainability bootstrap manifest is missing: %s\n' "$manifest" >&2
    exit 1
fi
if [[ ! -f "$lockfile" ]]; then
    printf 'maintainability bootstrap lockfile is missing: %s\n' "$lockfile" >&2
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

is_filesystem_root() {
    local directory=$1
    local parent
    parent=$(dirname -- "$directory")
    if [[ $parent == "$directory" ]]; then
        return 0
    fi
    if command -v cygpath >/dev/null 2>&1; then
        local windows_directory
        if windows_directory=$(cygpath -m "$directory" 2>/dev/null) && [[ $windows_directory =~ ^[[:alpha:]]:/$ ]]; then
            return 0
        fi
    fi
    return 1
}

directory=$repository_root
while :; do
    reject_cargo_config "$directory/.cargo"
    is_filesystem_root "$directory" && break
    directory=$(dirname -- "$directory")
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

sha256_file() {
    local path=$1
    local output
    if command -v sha256sum >/dev/null 2>&1; then
        output=$(sha256sum -- "$path")
    elif command -v shasum >/dev/null 2>&1; then
        output=$(shasum -a 256 -- "$path")
    else
        printf 'maintainability bootstrap requires sha256sum or shasum\n' >&2
        exit 1
    fi
    printf '%s\n' "${output%%[[:space:]]*}"
}

actual_manifest_sha256=$(sha256_file "$manifest")
if [[ $actual_manifest_sha256 != "$reviewed_manifest_sha256" ]]; then
    printf 'maintainability checker Cargo.toml does not match the reviewed dependency graph\n' >&2
    exit 1
fi

actual_lockfile_sha256=$(sha256_file "$lockfile")
if [[ $actual_lockfile_sha256 != "$reviewed_lockfile_sha256" ]]; then
    printf 'maintainability checker Cargo.lock does not match the reviewed dependency graph\n' >&2
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
