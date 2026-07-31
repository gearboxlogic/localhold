#!/usr/bin/bash
set -euo pipefail

if /usr/bin/env | /usr/bin/grep '^BASH_FUNC_' >/dev/null; then
    /usr/bin/printf 'maintainability bootstrap rejects inherited exported shell functions\n' >&2
    /usr/bin/false
fi

usage() {
    printf 'usage: check-maintainability-bootstrap.sh [--root PATH] [--source-safety|--dependency-unsafe|--maintainability|--test-environment]\n' >&2
    exit 1
}

script_path=${BASH_SOURCE[0]}
script_directory=${script_path%/*}
[[ $script_directory != "$script_path" ]] || script_directory=.
script_directory=$(cd -- "$script_directory" && pwd -P)
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
github_context_present=false
if [[ -v GITHUB_ACTIONS || -v GITHUB_EVENT_PATH || -v GITHUB_SHA ]]; then
    github_context_present=true
fi
governed_snapshot=$using_test_root
if $github_context_present; then
    governed_snapshot=true
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
readonly reviewed_runner_sha256=f9ead9aeff6aae855040ce3aea2e8901119071beef46061332dc3526378a9de6
readonly reviewed_bootstrap_tests_sha256=52ea4c035437422f778509e419598eec8843e087f9de730974630bc514ab6bfb
readonly reviewed_gate_runner_sha256=4283f980f6e785f50b52a6ce8c6968c2ceae2a5a9c61203f4a319df46b65d9d1

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
            BASH_ENV | GITHUB_PATH | LD_AUDIT | LD_LIBRARY_PATH | LD_PRELOAD | RUSTFLAGS | RUSTDOCFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_ENCODED_RUSTDOCFLAGS | RUSTC_BOOTSTRAP | CARGO_BUILD_TARGET | CARGO_TARGET_DIR | CLIPPY_ARGS | CLIPPY_CONF_DIR | \
                RUSTC | RUSTDOC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTDOC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | \
                CARGO_BUILD_RUSTFLAGS | CARGO_BUILD_RUSTDOCFLAGS | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | CARGO_TARGET_*_RUSTDOCFLAGS | \
                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER | GIT_* | TAR_OPTIONS)
                unset "$name"
                ;;
        esac
    done < <(compgen -e)
}

scrub_untrusted_environment
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

git_at() {
    local root=$1
    shift
    "$git_command" --no-replace-objects -c core.autocrlf=false -c diff.external= -C "$root" "$@"
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

    local first_parent
    first_parent=$(git_checked rev-parse --verify "${checked_head}^1^{commit}") || {
        printf 'GitHub checked revision has no trusted first parent\n' >&2
        exit 1
    }
    if [[ ${configured_base,,} != "${first_parent,,}" ]]; then
        printf 'configured maintainability base differs from the checked revision first parent\n' >&2
        exit 1
    fi
    printf '%s\n' "$first_parent"
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
    if $github_context_present; then
        if [[ ${GITHUB_ACTIONS:-} != true || -z ${GITHUB_EVENT_PATH:-} || -z ${GITHUB_SHA:-} ]]; then
            printf 'incomplete or invalid GitHub Actions environment cannot select a different checker revision\n' >&2
            exit 1
        fi
        if [[ ! $GITHUB_SHA =~ ^[[:xdigit:]]{40}$ || ${checked_head,,} != "${GITHUB_SHA,,}" ]]; then
            printf 'checked-out Git head revision differs from GITHUB_SHA before checker compilation\n' >&2
            exit 1
        fi
    fi

    local checker_revision=$checked_head
    if $using_test_root && $github_context_present; then
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
    done < <(git_checked ls-tree -r -z --full-tree "$checker_revision" -- tools/maintainability/src)
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

verify_reviewed_tracked_tree() {
    local checked_head
    checked_head=$(git_checked rev-parse --verify 'HEAD^{commit}') || {
        printf 'cannot read the checked-out revision before verifying governed inputs\n' >&2
        exit 1
    }

    local checker_sources_are_overlaid=false
    if $using_test_root && $github_context_present; then
        checker_sources_are_overlaid=true
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
        if $checker_sources_are_overlaid && [[ $relative_path == tools/maintainability/src/* ]]; then
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

if $governed_snapshot; then
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

    if ! $governed_snapshot; then
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
    if $github_context_present; then
        # Checker changes activate only after this revision lands. The trusted
        # first-parent checker audits the head tree without executing head code.
        trusted_checker_revision=$(trusted_github_base_revision "$checked_head")
        "$rm_command" -rf -- "$snapshot_root/tools/maintainability/src"
        git_at "$snapshot_root" archive --format=tar "$trusted_checker_revision" -- tools/maintainability/src |
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
    if [[ -w "$snapshot_root/tools/maintainability/src/main.rs" || ! -w "$snapshot_root/target" || ! -w "$audit_scratch_root" ]]; then
        printf 'maintainability source snapshot has invalid isolation permissions\n' >&2
        exit 1
    fi
    snapshot_bootstrap="$snapshot_root/script/check-maintainability-bootstrap.sh"
    "$bash_command" "$snapshot_bootstrap" --root "$snapshot_root"

    status=0
    "$bash_command" "$snapshot_root/script/run-maintainability-gate.sh" "$mode" || status=$?
    if (( status == 0 )); then
        "$bash_command" "$snapshot_bootstrap" --root "$snapshot_root" || status=$?
    fi
    preserve_audit_evidence || status=$?
    cleanup_snapshot
    trap - EXIT
    exit "$status"
fi
