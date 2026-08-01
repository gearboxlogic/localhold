#!/usr/bin/bash
set -euo pipefail
unset GCONV_PATH
unset OPENSSL_CONF OPENSSL_CONF_INCLUDE OPENSSL_ENGINES OPENSSL_MODULES
unset RIPGREP_CONFIG_PATH
CDPATH=
IFS=$' \t\n'
export -n CDPATH IFS

if /usr/bin/env | /usr/bin/grep '^BASH_FUNC_' >/dev/null; then
    /usr/bin/printf 'maintainability bootstrap rejects inherited exported shell functions\n' >&2
    exit 1 # inherited exported functions are unsupported
fi

usage() {
    printf 'usage: check-maintainability-bootstrap.sh [--root PATH] [--source-safety|--dependency-unsafe|--maintainability|--test-environment]\n' >&2
    exit 1
}

script_path=${BASH_SOURCE[0]}
script_directory=${script_path%/*}
[[ $script_directory != "$script_path" ]] || script_directory=.
script_directory=$(cd -- "$script_directory" && pwd -P)
script_path="$script_directory/${script_path##*/}"
implementation_root=$(cd -- "$script_directory/.." && pwd -P)
repository_root=$implementation_root
using_alternate_root=false
if [[ ${1:-} == --root ]]; then
    if (( $# < 2 )); then
        usage
    fi
    repository_root=$2
    using_alternate_root=true
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
github_context_present=false
if [[ -v GITHUB_ACTIONS || -v GITHUB_EVENT_PATH || -v GITHUB_SHA ]]; then
    github_context_present=true
fi
trusted_github_audit=false
if [[ $using_alternate_root == true && $github_context_present == true && $implementation_root != "$repository_root" ]]; then
    trusted_github_audit=true
fi
if [[ $using_alternate_root == true && $mode != verify && $mode != test-environment && $trusted_github_audit != true ]]; then
    printf 'alternate roots may run operational modes only from the protected GitHub workflow\n' >&2
    exit 1
fi
governed_snapshot=$using_alternate_root
if [[ $github_context_present == true ]]; then
    governed_snapshot=true
fi

reviewed_root=$repository_root
if [[ $trusted_github_audit == true ]]; then
    reviewed_root=$implementation_root
fi
tool_root="$reviewed_root/tools/maintainability"
source_root="$tool_root/src"
manifest="$tool_root/Cargo.toml"
lockfile="$tool_root/Cargo.lock"
justfile="$reviewed_root/Justfile"
mise_config="$reviewed_root/mise.toml"
mise_lockfile="$reviewed_root/mise.lock"
runner="$reviewed_root/script/run-source-safety.sh"
bootstrap_tests="$reviewed_root/script/tests/test_maintainability_bootstrap.sh"
gate_runner="$reviewed_root/script/run-maintainability-gate.sh"
readonly reviewed_manifest_sha256=cca207767614bd2c1d46bc06092b69e90157aeb450797fcc7cad4e1ed67c89b9
readonly reviewed_lockfile_sha256=825c6448351761aa5c4c6e1ce6b3696c927c4f46c5d43642846380d24f10467c
readonly reviewed_justfile_sha256=e7e0630e3bf9a4c042ab90c888fcdc46c3b9ccfd5c650d1b3fd69aa74c0df6f1
readonly reviewed_mise_config_sha256=627903d61cd155a318e0dffa4a29052099fbed1834bd485e7859fdcad03c0529
readonly reviewed_mise_lockfile_sha256=24a3c64cbd2123ba9ab457eba21a65c7960d189d6685fe1d2bfd4a979134c358
readonly reviewed_runner_sha256=cd756b8a6039e1192bb0c95e7c42e66148f7b883f3b12662b31c70269165a468
readonly reviewed_bootstrap_tests_sha256=1d84045741e53b3828427fa7279642ce44c7ab0eec7dcb7f867c69829e5fa48a
readonly reviewed_gate_runner_sha256=bccb831a2530705946afe1820a60f984dadc3b95b46b8ef88d21b1f7d67a8da4

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
            BASH_ENV | ENV | CDPATH | IFS | GCONV_PATH | GITHUB_PATH | LD_AUDIT | LD_LIBRARY_PATH | LD_PRELOAD | OPENSSL_CONF | OPENSSL_CONF_INCLUDE | OPENSSL_ENGINES | OPENSSL_MODULES | RIPGREP_CONFIG_PATH | RUSTFLAGS | RUSTDOCFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_ENCODED_RUSTDOCFLAGS | RUSTC_BOOTSTRAP | CARGO_BUILD_TARGET | CARGO_TARGET_DIR | CLIPPY_ARGS | CLIPPY_CONF_DIR | \
                RUSTC | RUSTDOC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTDOC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | \
                CARGO_BUILD_RUSTFLAGS | CARGO_BUILD_RUSTDOCFLAGS | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | CARGO_TARGET_*_RUSTDOCFLAGS | \
                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER | GIT_* | LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT | TAR_OPTIONS)
                unset "$name"
                ;;
        esac
    done < <(compgen -e)
}

scrub_untrusted_environment
GIT_CONFIG_NOSYSTEM=1
GIT_CONFIG_GLOBAL=/dev/null
readonly GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL
export GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL
git_command=$(trusted_system_command git)
find_command=$(trusted_system_command find)
mkdir_command=$(trusted_system_command mkdir)
mktemp_command=$(trusted_system_command mktemp)
rmdir_command=$(trusted_system_command rmdir)
rm_command=$(trusted_system_command rm)
mv_command=$(trusted_system_command mv)
chmod_command=$(trusted_system_command chmod)
tar_command=$(trusted_system_command tar)
bash_command=$(trusted_system_command bash)

has_write_mode_bits() {
    local path=$1
    [[ -n $("$find_command" "$path" -prune -perm /222 -print) ]]
}

git_at() {
    local root=$1
    shift
    "$git_command" --no-replace-objects -c core.autocrlf=false -c core.fsmonitor=false -c core.hooksPath=/dev/null -c diff.external= -C "$root" "$@"
}

git_checked() {
    git_at "$repository_root" "$@"
}

trusted_github_base_revision() {
    local checked_head=$1
    local configured_base=${LOCALHOLD_MAINTAINABILITY_BASE_REV:-}
    if [[ ! $configured_base =~ ^[[:xdigit:]]{40}$ ]]; then
        printf 'GitHub maintainability checks require a full base revision\n' >&2
        exit 1
    fi

    local trusted_base
    trusted_base=$(git_checked rev-parse --verify "${configured_base}^{commit}") || {
        printf 'GitHub maintainability base revision is unavailable\n' >&2
        exit 1
    }
    if [[ ${configured_base,,} != "${trusted_base,,}" || ${trusted_base,,} == "${checked_head,,}" ]]; then
        printf 'configured maintainability base must be a proper ancestor of the checked revision\n' >&2
        exit 1
    fi
    if ! git_checked merge-base --is-ancestor "$trusted_base" "$checked_head"; then
        printf 'configured maintainability base is not an ancestor of the checked revision\n' >&2
        exit 1
    fi
    printf '%s\n' "$trusted_base"
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
    if [[ $github_context_present == true ]]; then
        if [[ ${GITHUB_ACTIONS:-} != true || -z ${GITHUB_EVENT_PATH:-} || -z ${GITHUB_SHA:-} ]]; then
            printf 'incomplete or invalid GitHub Actions environment cannot select a different checker revision\n' >&2
            exit 1
        fi
        if [[ ! $GITHUB_SHA =~ ^[[:xdigit:]]{40}$ || ${checked_head,,} != "${GITHUB_SHA,,}" ]]; then
            printf 'checked-out Git head revision differs from GITHUB_SHA before checker compilation\n' >&2
            exit 1
        fi
    fi

    local checker_root=$repository_root
    local checker_git_root=$repository_root
    local checker_revision=$checked_head
    if [[ $trusted_github_audit == true ]]; then
        checker_root=$implementation_root
        checker_git_root=$implementation_root
        checker_revision=$(git_at "$implementation_root" rev-parse --verify 'HEAD^{commit}') || {
            printf 'cannot authenticate the protected maintainability implementation revision\n' >&2
            exit 1
        }
    elif [[ $using_alternate_root == true && $github_context_present == true ]]; then
        checker_revision=$(trusted_github_base_revision "$checked_head")
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
        if [[ ! -f "$checker_root/$relative_path" || -L "$checker_root/$relative_path" ]]; then
            printf 'maintainability checker source must be a regular non-symlink file: %s\n' "$relative_path" >&2
            exit 1
        fi
        actual_hash=$(git_at "$checker_git_root" hash-object --no-filters -- "$checker_root/$relative_path") || {
            printf 'cannot hash maintainability checker source: %s\n' "$relative_path" >&2
            exit 1
        }
        if [[ $actual_hash != "$expected_hash" ]]; then
            printf 'maintainability checker source differs from the checked-out revision: %s\n' "$relative_path" >&2
            exit 1
        fi
        expected_paths["$relative_path"]=1
        ((expected_count += 1))
    done < <(git_at "$checker_git_root" ls-tree -r -z --full-tree "$checker_revision" -- tools/maintainability/src)
    if (( expected_count == 0 )); then
        printf 'checked-out revision contains no maintainability checker sources\n' >&2
        exit 1
    fi

    local observed_count=0
    local observed_path
    while IFS= read -r -d '' observed_path; do
        relative_path=${observed_path#"$checker_root/"}
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

verify_reviewed_tracked_tree() {
    local checked_head
    checked_head=$(git_checked rev-parse --verify 'HEAD^{commit}') || {
        printf 'cannot read the checked-out revision before verifying governed inputs\n' >&2
        exit 1
    }

    local checker_inputs_are_overlaid=false
    if [[ $using_alternate_root == true && $github_context_present == true && $trusted_github_audit != true ]]; then
        checker_inputs_are_overlaid=true
    fi

    local -A expected_index_entries=()
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
            printf 'checked-out revision contains an unsupported tracked entry: %s\n' "$relative_path" >&2
            exit 1
        fi
        if [[ $checker_inputs_are_overlaid == true &&
            ( $relative_path == tools/maintainability/src/* ||
                $relative_path == tools/maintainability/Cargo.toml ||
                $relative_path == tools/maintainability/Cargo.lock ) ]]; then
            :
        else
            if [[ ! -f "$repository_root/$relative_path" || -L "$repository_root/$relative_path" ]]; then
                printf 'reviewed tracked input must be a regular non-symlink file: %s\n' "$relative_path" >&2
                exit 1
            fi
            actual_hash=$(git_checked hash-object --no-filters -- "$repository_root/$relative_path") || {
                printf 'cannot hash reviewed tracked input: %s\n' "$relative_path" >&2
                exit 1
            }
            if [[ $actual_hash != "$expected_hash" ]]; then
                printf 'reviewed tracked input differs from the checked-out revision: %s\n' "$relative_path" >&2
                exit 1
            fi
        fi
        expected_index_entries["$relative_path"]="$mode $expected_hash"
        ((expected_count += 1))
    done < <(git_checked ls-tree -r -z --full-tree "$checked_head")
    if (( expected_count == 0 )); then
        printf 'checked-out revision contains no reviewed tracked inputs\n' >&2
        exit 1
    fi

    local indexed_count=0
    local stage
    while IFS= read -r -d '' entry; do
        metadata=${entry%%$'\t'*}
        relative_path=${entry#*$'\t'}
        read -r mode expected_hash stage <<<"$metadata"
        if [[ $stage != 0 || $mode != 100644 && $mode != 100755 ]]; then
            printf 'index contains an unsupported tracked entry: %s\n' "$relative_path" >&2
            exit 1
        fi
        if [[ ${expected_index_entries["$relative_path"]:-} != "$mode $expected_hash" ]]; then
            printf 'index differs from the checked-out revision: %s\n' "$relative_path" >&2
            exit 1
        fi
        ((indexed_count += 1))
    done < <(git_checked ls-files -z --stage)
    if (( indexed_count != expected_count )); then
        printf 'index path set differs from the checked-out revision\n' >&2
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

sha256_revision_file() {
    local revision=$1
    local relative_path=$2
    local output
    output=$(git_at "$repository_root" show "$revision:$relative_path" | "$sha256_command") || {
        printf 'cannot hash trusted checker input: %s\n' "$relative_path" >&2
        return 1
    }
    printf '%s\n' "${output%%[[:space:]]*}"
}

expected_manifest_sha256=$reviewed_manifest_sha256
expected_lockfile_sha256=$reviewed_lockfile_sha256
if [[ $using_alternate_root == true && $github_context_present == true && $trusted_github_audit != true ]]; then
    checked_head=$(git_checked rev-parse --verify 'HEAD^{commit}') || {
        printf 'cannot read the checked-out revision before verifying the checker dependency graph\n' >&2
        exit 1
    }
    trusted_checker_revision=$(trusted_github_base_revision "$checked_head")
    expected_manifest_sha256=$(sha256_revision_file "$trusted_checker_revision" tools/maintainability/Cargo.toml)
    expected_lockfile_sha256=$(sha256_revision_file "$trusted_checker_revision" tools/maintainability/Cargo.lock)
fi

actual_manifest_sha256=$(sha256_file "$manifest")
if [[ $actual_manifest_sha256 != "$expected_manifest_sha256" ]]; then
    printf 'maintainability checker Cargo.toml does not match the reviewed dependency graph\n' >&2
    exit 1
fi

actual_lockfile_sha256=$(sha256_file "$lockfile")
if [[ $actual_lockfile_sha256 != "$expected_lockfile_sha256" ]]; then
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

if [[ $governed_snapshot == true ]]; then
    verify_checker_sources
    verify_reviewed_tracked_tree
fi

printf 'maintainability bootstrap check passed\n'

if [[ $mode != verify ]]; then
    git_executable=$git_command
    if [[ $OSTYPE == msys* || $OSTYPE == cygwin* ]]; then
        cygpath_command=$(trusted_system_command cygpath)
        git_executable=$("$cygpath_command" -w "$git_executable")
    fi
    LOCALHOLD_MAINTAINABILITY_GIT=$git_executable
    export LOCALHOLD_MAINTAINABILITY_GIT

    if [[ $governed_snapshot != true ]]; then
        "$bash_command" "$gate_runner" "$mode"
        exit
    fi

    checked_head=$(git_checked rev-parse --verify 'HEAD^{commit}')
    target_parent="$repository_root/target"
    if [[ -L $target_parent || -e $target_parent && ! -d $target_parent ]]; then
        printf 'maintainability snapshot parent must be a regular non-symlink directory\n' >&2
        exit 1
    fi
    if [[ ! -d $target_parent ]]; then
        "$mkdir_command" -- "$target_parent"
    fi
    target_parent=$(cd -- "$target_parent" && pwd -P)
    if [[ $target_parent != "$repository_root/target" ]]; then
        printf 'maintainability snapshot parent resolves outside the repository target directory\n' >&2
        exit 1
    fi

    umask 077
    # Keep this private prefix short: nested dependency builds on Windows can
    # still encounter MAX_PATH after adding their own temporary directories.
    snapshot_root=$("$mktemp_command" -d "$target_parent/s.XXXXXXXX")
    "$rmdir_command" -- "$snapshot_root"
    cleanup_snapshot() {
        if [[ -n ${snapshot_root:-} && -e $snapshot_root ]]; then
            "$chmod_command" -R u+w -- "$snapshot_root" 2>/dev/null || true
            "$rm_command" -rf -- "$snapshot_root"
        fi
    }
    preserve_audit_evidence() {
        local snapshot_evidence_parent="$snapshot_root/target/dependency-unsafe"
        if [[ ! -e $snapshot_evidence_parent && ! -L $snapshot_evidence_parent ]]; then
            return 0
        fi
        if [[ ! -d $snapshot_evidence_parent || -L $snapshot_evidence_parent ]]; then
            printf 'maintainability audit evidence parent must be a regular directory\n' >&2
            return 1
        fi

        local evidence_parent="$target_parent/dependency-unsafe"
        if [[ -L $evidence_parent || -e $evidence_parent && ! -d $evidence_parent ]]; then
            printf 'maintainability durable evidence parent must be a regular non-symlink directory\n' >&2
            return 1
        fi
        if [[ ! -d $evidence_parent ]]; then
            "$mkdir_command" -- "$evidence_parent" || return
        fi
        evidence_parent=$(cd -- "$evidence_parent" && pwd -P)
        if [[ $evidence_parent != "$target_parent/dependency-unsafe" ]]; then
            printf 'maintainability durable evidence parent resolves outside the repository target directory\n' >&2
            return 1
        fi

        local evidence
        local evidence_name
        local destination
        for evidence in "$snapshot_evidence_parent"/actual-*; do
            if [[ ! -e $evidence && ! -L $evidence ]]; then
                continue
            fi
            if [[ ! -d $evidence || -L $evidence ]]; then
                printf 'maintainability audit evidence must be a regular non-symlink directory\n' >&2
                return 1
            fi
            evidence_name=${evidence##*/}
            destination="$evidence_parent/$evidence_name"
            if [[ -e $destination || -L $destination ]]; then
                "$rm_command" -rf -- "$destination" || return
            fi
            "$mv_command" -- "$evidence" "$destination" || return
        done
    }
    trap cleanup_snapshot EXIT

    git_checked clone --no-hardlinks --no-checkout --quiet -- "$repository_root" "$snapshot_root"
    git_at "$snapshot_root" update-ref --no-deref HEAD "$checked_head"
    git_at "$snapshot_root" read-tree "$checked_head"
    git_at "$snapshot_root" archive --format=tar "$checked_head" | "$tar_command" -xf - -C "$snapshot_root"
    if [[ $github_context_present == true && $trusted_github_audit != true ]]; then
        # Checker changes activate only after this revision lands. The trusted
        # event-base checker graph audits the head tree without executing head code.
        trusted_checker_revision=$(trusted_github_base_revision "$checked_head")
        "$rm_command" -rf -- "$snapshot_root/tools/maintainability/Cargo.toml" "$snapshot_root/tools/maintainability/Cargo.lock" \
            "$snapshot_root/tools/maintainability/src"
        git_at "$snapshot_root" archive --format=tar "$trusted_checker_revision" -- tools/maintainability/Cargo.toml \
            tools/maintainability/Cargo.lock tools/maintainability/src |
            "$tar_command" -xf - -C "$snapshot_root"
    fi
    audit_scratch_root="$snapshot_root/.cache"
    "$mkdir_command" -- "$snapshot_root/target" "$audit_scratch_root"
    # Governed CI permits only pinned actions before this first repository
    # command. These mode bits are additional accidental-mutation protection;
    # the closed workflow step sequence is the process-isolation boundary.
    "$chmod_command" -R a-w -- "$snapshot_root"
    # The dependency audit owns .cache/dependency-unsafe for confined scratch
    # space; all durable evidence remains under the separately writable target.
    "$chmod_command" u+rwx -- "$snapshot_root/target" "$audit_scratch_root"
    isolation_probe="$snapshot_root/src/lib.rs"
    if [[ ! -f $isolation_probe || -L $isolation_probe ]] ||
        has_write_mode_bits "$isolation_probe" ||
        ! has_write_mode_bits "$snapshot_root/target" ||
        ! has_write_mode_bits "$audit_scratch_root"; then
        printf 'maintainability source snapshot has invalid isolation permissions\n' >&2
        exit 1
    fi
    snapshot_bootstrap="$snapshot_root/script/check-maintainability-bootstrap.sh"
    snapshot_gate_runner="$snapshot_root/script/run-maintainability-gate.sh"
    if [[ $trusted_github_audit == true ]]; then
        snapshot_bootstrap=$script_path
        snapshot_gate_runner=$gate_runner
    fi
    "$bash_command" "$snapshot_bootstrap" --root "$snapshot_root"

    status=0
    if [[ $trusted_github_audit == true ]]; then
        LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT=$snapshot_root "$bash_command" "$snapshot_gate_runner" "$mode" || status=$?
    else
        "$bash_command" "$snapshot_gate_runner" "$mode" || status=$?
    fi
    if (( status == 0 )); then
        "$bash_command" "$snapshot_bootstrap" --root "$snapshot_root" || status=$?
    fi
    preserve_audit_evidence || status=$?
    cleanup_snapshot
    trap - EXIT
    exit "$status"
fi
