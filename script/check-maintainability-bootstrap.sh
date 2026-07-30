#!/usr/bin/bash
set -euo pipefail

usage() {
    printf 'usage: check-maintainability-bootstrap.sh [--root PATH] [--source-safety|--dependency-unsafe|--maintainability|--test-environment]\n' >&2
    exit 1
}

script_path=${BASH_SOURCE[0]}
script_directory=${script_path%/*}
[[ $script_directory != "$script_path" ]] || script_directory=.
repository_root=$(cd -- "$script_directory/.." && pwd -P)
using_test_root=false
if [[ ${1:-} == --root ]]; then
    if (( $# < 2 )); then
        usage
    fi
    repository_root=$2
    using_test_root=true
    shift 2
fi
repository_root=$(cd -- "$repository_root" && pwd -P)

mode=verify
if (( $# > 0 )); then
    case "$1" in
        --source-safety) mode=source-safety ;;
        --dependency-unsafe) mode=dependency-unsafe ;;
        --maintainability) mode=maintainability ;;
        --test-environment) mode=test-environment ;;
        *) usage ;;
    esac
    shift
fi
(( $# == 0 )) || usage
if $using_test_root && [[ $mode != verify && $mode != test-environment ]]; then
    printf 'alternate roots may only verify the bootstrap or its environment\n' >&2
    exit 1
fi

tool_root="$repository_root/tools/maintainability"
source_root="$tool_root/src"
manifest="$tool_root/Cargo.toml"
lockfile="$tool_root/Cargo.lock"
justfile="$repository_root/Justfile"
mise_config="$repository_root/mise.toml"
mise_lockfile="$repository_root/mise.lock"
runner="$repository_root/script/run-source-safety.sh"
bootstrap_tests="$repository_root/script/tests/test_maintainability_bootstrap.sh"
gate_runner="$repository_root/script/run-maintainability-gate.sh"
readonly reviewed_manifest_sha256=cca207767614bd2c1d46bc06092b69e90157aeb450797fcc7cad4e1ed67c89b9
readonly reviewed_lockfile_sha256=825c6448351761aa5c4c6e1ce6b3696c927c4f46c5d43642846380d24f10467c
readonly reviewed_justfile_sha256=e7e0630e3bf9a4c042ab90c888fcdc46c3b9ccfd5c650d1b3fd69aa74c0df6f1
readonly reviewed_mise_config_sha256=627903d61cd155a318e0dffa4a29052099fbed1834bd485e7859fdcad03c0529
readonly reviewed_mise_lockfile_sha256=24a3c64cbd2123ba9ab457eba21a65c7960d189d6685fe1d2bfd4a979134c358
readonly reviewed_runner_sha256=09c7b7cc9472acc7ec19633a9ccbf54eb7cf66215ec5921ade3cbd3eacd5eb1e
readonly reviewed_bootstrap_tests_sha256=4be8401d8db70eff00160e298f051efc2c44513118fb6c9994963bf44a74dceb
readonly reviewed_gate_runner_sha256=32fd3a1598da87212ac0622cc2df4be79a8c562f9efbdc316d8405189404512d

for reviewed_path in "$manifest" "$lockfile" "$justfile" "$mise_config" "$mise_lockfile" "$runner" "$bootstrap_tests" "$gate_runner"; do
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
        cygpath_command=$(trusted_system_command cygpath)
        if windows_directory=$("$cygpath_command" -m "$directory" 2>/dev/null) && [[ $windows_directory =~ ^[[:alpha:]]:/$ ]]; then
            return 0
        fi
    fi
    return 1
}

trusted_system_command() {
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
    case "$directory" in
        /bin | /usr/bin | /mingw64/bin) ;;
        *)
            printf 'maintainability bootstrap requires an OS-owned command: %s\n' "$candidate" >&2
            exit 1
            ;;
    esac
    if [[ ! -f "$candidate" || ! -x "$candidate" ]]; then
        printf 'maintainability bootstrap requires an executable system file: %s\n' "$candidate" >&2
        exit 1
    fi
    printf '%s\n' "$candidate"
}

scrub_untrusted_environment() {
    local name
    local uppercase
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
}

scrub_untrusted_environment
git_command=$(trusted_system_command git)
find_command=$(trusted_system_command find)

git_checked() {
    "$git_command" --no-replace-objects -c diff.external= -C "$repository_root" "$@"
}

verify_checker_sources() {
    if [[ ! -d "$source_root" || -L "$source_root" ]]; then
        printf 'maintainability checker source root must be a regular directory: %s\n' "$source_root" >&2
        exit 1
    fi

    local checked_head
    checked_head=$(git_checked rev-parse --verify 'HEAD^{commit}') || {
        printf 'cannot read the checked-out revision before compiling the maintainability checker\n' >&2
        exit 1
    }
    if [[ -v GITHUB_ACTIONS || -v GITHUB_EVENT_PATH || -v GITHUB_SHA ]]; then
        if [[ ${GITHUB_ACTIONS:-} != true || -z ${GITHUB_EVENT_PATH:-} || -z ${GITHUB_SHA:-} ]]; then
            printf 'incomplete or invalid GitHub Actions environment cannot select a different checker revision\n' >&2
            exit 1
        fi
        if [[ ! $GITHUB_SHA =~ ^[[:xdigit:]]{40}$ || ${checked_head,,} != "${GITHUB_SHA,,}" ]]; then
            printf 'checked-out Git head revision differs from GITHUB_SHA before checker compilation\n' >&2
            exit 1
        fi
    fi

    local -A expected_paths=()
    local expected_count=0
    local entry
    local metadata
    local mode
    local object_type
    local expected_hash
    local relative_path
    local actual_hash
    while IFS= read -r -d '' entry; do
        metadata=${entry%%$'\t'*}
        relative_path=${entry#*$'\t'}
        read -r mode object_type expected_hash <<<"$metadata"
        if [[ $object_type != blob || $mode != 100644 && $mode != 100755 ]]; then
            printf 'maintainability checker source revision contains an unsupported entry: %s\n' "$relative_path" >&2
            exit 1
        fi
        if [[ ! -f "$repository_root/$relative_path" || -L "$repository_root/$relative_path" ]]; then
            printf 'maintainability checker source must be a regular non-symlink file: %s\n' "$relative_path" >&2
            exit 1
        fi
        actual_hash=$(git_checked hash-object --no-filters -- "$repository_root/$relative_path") || {
            printf 'cannot hash maintainability checker source: %s\n' "$relative_path" >&2
            exit 1
        }
        if [[ $actual_hash != "$expected_hash" ]]; then
            printf 'maintainability checker source differs from the checked-out revision: %s\n' "$relative_path" >&2
            exit 1
        fi
        expected_paths["$relative_path"]=1
        ((expected_count += 1))
    done < <(git_checked ls-tree -r -z --full-tree "$checked_head" -- tools/maintainability/src)
    if (( expected_count == 0 )); then
        printf 'checked-out revision contains no maintainability checker sources\n' >&2
        exit 1
    fi

    local observed_count=0
    local observed_path
    while IFS= read -r -d '' observed_path; do
        relative_path=${observed_path#"$repository_root/"}
        if [[ -z ${expected_paths["$relative_path"]+present} ]]; then
            printf 'maintainability checker source is absent from the checked-out revision: %s\n' "$relative_path" >&2
            exit 1
        fi
        ((observed_count += 1))
    done < <("$find_command" "$source_root" \( -type f -o -type l \) -print0)
    if (( observed_count != expected_count )); then
        printf 'maintainability checker source set differs from the checked-out revision\n' >&2
        exit 1
    fi
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
    cygpath_command=$(trusted_system_command cygpath)
    cargo_home=$("$cygpath_command" -u "$cargo_home")
fi
if [[ -n $cargo_home ]]; then
    reject_cargo_config "$cargo_home"
fi

awk_command=$(trusted_system_command awk)
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

sha256_command=$(trusted_system_command sha256sum)

sha256_file() {
    local path=$1
    local output
    output=$("$sha256_command" -- "$path")
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

actual_justfile_sha256=$(sha256_file "$justfile")
if [[ $actual_justfile_sha256 != "$reviewed_justfile_sha256" ]]; then
    printf 'Justfile does not match the reviewed maintainability dispatcher\n' >&2
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

actual_bootstrap_tests_sha256=$(sha256_file "$bootstrap_tests")
if [[ $actual_bootstrap_tests_sha256 != "$reviewed_bootstrap_tests_sha256" ]]; then
    printf 'maintainability bootstrap tests do not match the reviewed test driver\n' >&2
    exit 1
fi

actual_gate_runner_sha256=$(sha256_file "$gate_runner")
if [[ $actual_gate_runner_sha256 != "$reviewed_gate_runner_sha256" ]]; then
    printf 'maintainability gate runner does not match the reviewed fixed dispatcher\n' >&2
    exit 1
fi

verify_checker_sources

printf 'maintainability bootstrap check passed\n'

if [[ $mode != verify ]]; then
    git_executable=$git_command
    if [[ $OSTYPE == msys* || $OSTYPE == cygwin* ]]; then
        cygpath_command=$(trusted_system_command cygpath)
        git_executable=$("$cygpath_command" -w "$git_executable")
    fi
    LOCALHOLD_MAINTAINABILITY_GIT=$git_executable
    export LOCALHOLD_MAINTAINABILITY_GIT
    bash_command=$(trusted_system_command bash)
    exec "$bash_command" "$gate_runner" "$mode"
fi
