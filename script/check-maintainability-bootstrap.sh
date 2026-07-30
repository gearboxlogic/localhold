#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_directory=${script_path%/*}
[[ $script_directory != "$script_path" ]] || script_directory=.
repository_root=$(cd -- "$script_directory/.." && pwd -P)
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
mise_config="$repository_root/mise.toml"
mise_lockfile="$repository_root/mise.lock"
runner="$repository_root/script/run-source-safety.sh"
readonly reviewed_manifest_sha256=cca207767614bd2c1d46bc06092b69e90157aeb450797fcc7cad4e1ed67c89b9
readonly reviewed_lockfile_sha256=825c6448351761aa5c4c6e1ce6b3696c927c4f46c5d43642846380d24f10467c
readonly reviewed_mise_config_sha256=627903d61cd155a318e0dffa4a29052099fbed1834bd485e7859fdcad03c0529
readonly reviewed_mise_lockfile_sha256=24a3c64cbd2123ba9ab457eba21a65c7960d189d6685fe1d2bfd4a979134c358
readonly reviewed_runner_sha256=09c7b7cc9472acc7ec19633a9ccbf54eb7cf66215ec5921ade3cbd3eacd5eb1e

for reviewed_path in "$manifest" "$lockfile" "$mise_config" "$mise_lockfile" "$runner"; do
    if [[ ! -f "$reviewed_path" || -L "$reviewed_path" ]]; then
        printf 'reviewed maintainability bootstrap input must be a regular non-symlink file: %s\n' "$reviewed_path" >&2
        exit 1
    fi
done

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
    if [[ $directory == / || $directory == // ]]; then
        return 0
    fi
    local parent
    parent=$(cd -- "$directory/.." && pwd -P)
    if [[ $parent == "$directory" ]]; then
        return 0
    fi
    if command -v cygpath >/dev/null 2>&1; then
        local windows_directory
        local cygpath_command
        cygpath_command=$(trusted_command_path cygpath)
        if windows_directory=$("$cygpath_command" -m "$directory" 2>/dev/null) && [[ $windows_directory =~ ^[[:alpha:]]:/$ ]]; then
            return 0
        fi
    fi
    return 1
}

trusted_command_path() {
    local name=$1
    local candidate
    candidate=$(type -P -- "$name") || {
        printf 'maintainability bootstrap requires command: %s\n' "$name" >&2
        exit 1
    }
    local directory=${candidate%/*}
    [[ $directory != "$candidate" ]] || directory=.
    directory=$(cd -- "$directory" && pwd -P)
    candidate="$directory/${candidate##*/}"
    if [[ $candidate == "$repository_root" || $candidate == "$repository_root/"* ]]; then
        printf 'maintainability bootstrap refuses a repository-controlled command: %s\n' "$candidate" >&2
        exit 1
    fi
    printf '%s\n' "$candidate"
}

directory=$repository_root
while :; do
    reject_cargo_config "$directory/.cargo"
    is_filesystem_root "$directory" && break
    directory=$(cd -- "$directory/.." && pwd -P)
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
    cygpath_command=$(trusted_command_path cygpath)
    cargo_home=$("$cygpath_command" -u "$cargo_home")
fi
if [[ -n $cargo_home ]]; then
    reject_cargo_config "$cargo_home"
fi

awk_command=$(trusted_command_path awk)
build_setting=$(
    "$awk_command" '
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
        local sha256_command
        sha256_command=$(trusted_command_path sha256sum)
        output=$("$sha256_command" -- "$path")
    elif command -v shasum >/dev/null 2>&1; then
        local shasum_command
        shasum_command=$(trusted_command_path shasum)
        output=$("$shasum_command" -a 256 -- "$path")
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

actual_mise_config_sha256=$(sha256_file "$mise_config")
if [[ $actual_mise_config_sha256 != "$reviewed_mise_config_sha256" ]]; then
    printf 'mise.toml does not match the reviewed maintainability tool environment\n' >&2
    exit 1
fi

actual_mise_lockfile_sha256=$(sha256_file "$mise_lockfile")
if [[ $actual_mise_lockfile_sha256 != "$reviewed_mise_lockfile_sha256" ]]; then
    printf 'mise.lock does not match the reviewed maintainability tool environment\n' >&2
    exit 1
fi

actual_runner_sha256=$(sha256_file "$runner")
if [[ $actual_runner_sha256 != "$reviewed_runner_sha256" ]]; then
    printf 'run-source-safety.sh does not match the reviewed bootstrap runner\n' >&2
    exit 1
fi

printf 'maintainability bootstrap check passed\n'

if (( ${#command[@]} > 0 )); then
    LOCALHOLD_MAINTAINABILITY_CARGO=$(trusted_command_path cargo)
    git_executable=$(trusted_command_path git)
    if [[ $OSTYPE == msys* || $OSTYPE == cygwin* ]]; then
        cygpath_command=$(trusted_command_path cygpath)
        git_executable=$("$cygpath_command" -w "$git_executable")
    fi
    LOCALHOLD_MAINTAINABILITY_GIT=$git_executable
    export LOCALHOLD_MAINTAINABILITY_CARGO LOCALHOLD_MAINTAINABILITY_GIT
    while IFS= read -r name; do
        uppercase=${name^^}
        case "$uppercase" in
            RUSTFLAGS | RUSTDOCFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_ENCODED_RUSTDOCFLAGS | RUSTC_BOOTSTRAP | CARGO_BUILD_TARGET | CLIPPY_ARGS | CLIPPY_CONF_DIR | \
                RUSTC | RUSTDOC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTDOC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | \
                CARGO_BUILD_RUSTFLAGS | CARGO_BUILD_RUSTDOCFLAGS | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | CARGO_TARGET_*_RUSTDOCFLAGS | \
                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER | GIT_*)
                unset "$name"
                ;;
        esac
    done < <(compgen -e)
    exec "${command[@]}"
fi
