#!/usr/bin/bash
set -euo pipefail

unset GITHUB_ACTIONS GITHUB_EVENT_PATH GITHUB_SHA LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT LOCALHOLD_MAINTAINABILITY_BASE_REV

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
check="$repository_root/script/check-maintainability-bootstrap.sh"
ci_workflow="$repository_root/.github/workflows/ci.yml"
fixture_parent="$repository_root/target/bootstrap-tests"
mkdir -p "$fixture_parent"
fixture=$(mktemp -d "$fixture_parent/run.XXXXXXXX")
trap 'rm -rf -- "$fixture"; rmdir -- "$fixture_parent" 2>/dev/null || true' EXIT
test_git_config_home="$fixture/git-config"
mkdir -p "$test_git_config_home/git"
printf '[core]\n\tautocrlf = true\n' >"$test_git_config_home/git/config"
XDG_CONFIG_HOME=$test_git_config_home
export XDG_CONFIG_HOME
test_repository="$fixture/repository"
mkdir -p "$test_repository/tools/maintainability"
source_tool="$repository_root/tools/maintainability"
test_tool="$test_repository/tools/maintainability"
source_runner="$repository_root/script/run-source-safety.sh"
source_bootstrap_tests="$repository_root/script/tests/test_maintainability_bootstrap.sh"
source_gate_runner="$repository_root/script/run-maintainability-gate.sh"

sha256_stream() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256
    else
        printf 'maintainability bootstrap tests require sha256sum or shasum\n' >&2
        exit 1
    fi
}

has_write_mode_bits() {
    [[ -n $(find "$1" -prune -perm /222 -print) ]]
}

bootstrap_file_sha256=$(sha256_stream <"$check")
bootstrap_file_sha256=${bootstrap_file_sha256%%[[:space:]]*}
bootstrap_digest_bytes=$(printf '%s\n' "$bootstrap_file_sha256" | sed 's/../\\x&/g')
bootstrap_sha256=$(printf '%b' "$bootstrap_digest_bytes" | sha256_stream)
bootstrap_sha256=${bootstrap_sha256%%[[:space:]]*}
workflow_sha256=$(sed -n 's/^[[:space:]]*LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256: //p' "$ci_workflow")
if [[ $workflow_sha256 != "$bootstrap_sha256" ]]; then
    printf 'CI maintainability bootstrap digest is stale\n' >&2
    exit 1
fi
guard_count=$(grep -Fc 'if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then' "$ci_workflow" || true)
if (( guard_count != 2 )); then
    printf 'every CI maintainability bootstrap execution must have an immediate workflow digest guard\n' >&2
    exit 1
fi
for loader_variable in LD_AUDIT LD_LIBRARY_PATH LD_PRELOAD; do
    loader_guard_count=$(grep -Fc "          $loader_variable: ''" "$ci_workflow" || true)
    if (( loader_guard_count != 2 )); then
        printf 'every CI maintainability bootstrap execution must clear %s before Bash starts\n' "$loader_variable" >&2
        exit 1
    fi
done

write_manifest() {
    printf '%s\n' "$1" >"$test_tool/Cargo.toml"
}

restore_reviewed_graph() {
    cp "$source_tool/Cargo.toml" "$test_tool/Cargo.toml"
    cp "$source_tool/Cargo.lock" "$test_tool/Cargo.lock"
    rm -rf "$test_tool/src"
    cp -R "$source_tool/src" "$test_tool/src"
    cp "$repository_root/Justfile" "$test_repository/Justfile"
    cp "$repository_root/mise.toml" "$test_repository/mise.toml"
    cp "$repository_root/mise.lock" "$test_repository/mise.lock"
    mkdir -p "$test_repository/script"
    cp "$check" "$test_repository/script/check-maintainability-bootstrap.sh"
    cp "$source_runner" "$test_repository/script/run-source-safety.sh"
    cp "$source_gate_runner" "$test_repository/script/run-maintainability-gate.sh"
    printf '%s\n' '#!/usr/bin/bash' 'printf reviewed-command\\n' >"$test_repository/script/reviewed-command.sh"
    mkdir -p "$test_repository/script/tests"
    cp "$source_bootstrap_tests" "$test_repository/script/tests/test_maintainability_bootstrap.sh"
    mkdir -p "$test_repository/src"
    printf 'pub fn reviewed() {}\n' >"$test_repository/src/lib.rs"
    chmod -R u+w -- "$test_repository"
}

run_check() {
    "$check" --root "$test_repository" "$@"
}

run_local_check() {
    "$test_repository/script/check-maintainability-bootstrap.sh" "$@"
}

expect_failure() {
    if run_check >/dev/null 2>&1; then
        printf 'maintainability bootstrap fixture unexpectedly passed\n' >&2
        exit 1
    fi
}

expect_failure_before_command() {
    if run_check --test-environment >/dev/null 2>&1; then
        printf 'maintainability bootstrap fixture unexpectedly passed\n' >&2
        exit 1
    fi
}

restore_reviewed_graph
git -C "$test_repository" init -q
git -C "$test_repository" -c core.autocrlf=false -c user.name=LocalHold -c user.email=localhold@example.invalid add .
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'reviewed fixture'
test_base=$(git -C "$test_repository" rev-parse HEAD)
run_check >/dev/null

inherited_config_marker="$fixture/global-git-config-ran"
inherited_config_helper="$fixture/global-git-config-helper"
printf '%s\n' '#!/usr/bin/bash' 'printf executed >"$GLOBAL_GIT_CONFIG_MARKER"' >"$inherited_config_helper"
chmod +x "$inherited_config_helper"
printf '[core]\n\tautocrlf = true\n\tfsmonitor = sh %s\n' "$inherited_config_helper" >"$test_git_config_home/git/config"
GLOBAL_GIT_CONFIG_MARKER=$inherited_config_marker run_check --test-environment >/dev/null
if [[ -e $inherited_config_marker ]]; then
    printf 'maintainability bootstrap executed inherited global Git configuration\n' >&2
    exit 1
fi
printf '[core]\n\tautocrlf = true\n' >"$test_git_config_home/git/config"

printf 'pub fn locally_changed() {}\n' >"$test_repository/src/lib.rs"
run_local_check >/dev/null
run_local_check --test-environment >/dev/null
restore_reviewed_graph

(
    for _ in {1..1000}; do
        snapshot_candidates=("$test_repository"/target/s.*)
        snapshot=${snapshot_candidates[0]}
        if [[ -d $snapshot/target ]] &&
            has_write_mode_bits "$snapshot/target" &&
            ! has_write_mode_bits "$snapshot/tools/maintainability/src/main.rs"; then
            if mkdir -p "$snapshot/target/dependency-unsafe/actual-test" 2>/dev/null; then
                printf 'preserved evidence\n' >"$snapshot/target/dependency-unsafe/actual-test/evidence.txt"
                printf '#![allow(warnings)]\npub fn changed_after_verification() {}\n' >"$test_repository/src/lib.rs"
                exit 0
            fi
        fi
        sleep 0.01
    done
    exit 1
) &
snapshot_mutator_pid=$!
snapshot_status=0
run_check --test-environment >/dev/null || snapshot_status=$?
if ! wait "$snapshot_mutator_pid"; then
    printf 'maintainability bootstrap did not create an isolated source snapshot\n' >&2
    exit 1
fi
if (( snapshot_status != 0 )); then
    printf 'maintainability bootstrap used the mutable working tree after verification\n' >&2
    exit 1
fi
if compgen -G "$test_repository/target/s.*" >/dev/null; then
    printf 'maintainability bootstrap retained an isolated source snapshot\n' >&2
    exit 1
fi
if [[ $(<"$test_repository/target/dependency-unsafe/actual-test/evidence.txt") != 'preserved evidence' ]]; then
    printf 'maintainability bootstrap did not preserve dependency audit evidence outside its source snapshot\n' >&2
    exit 1
fi
restore_reviewed_graph

printf '#![allow(warnings)]\npub fn reviewed() {}\n' >"$test_repository/src/lib.rs"
expect_failure_before_command

restore_reviewed_graph
printf '%s\n' '#!/usr/bin/bash' 'printf changed-command\\n' >"$test_repository/script/reviewed-command.sh"
expect_failure_before_command

restore_reviewed_graph
printf 'fn main() {}\n' >"$test_tool/src/main.rs"
expect_failure_before_command

restore_reviewed_graph
mkdir -p "$test_tool/src/bin"
printf 'fn main() {}\n' >"$test_tool/src/bin/unreviewed.rs"
expect_failure_before_command

restore_reviewed_graph
event_path="$fixture/event.json"
printf '{}\n' >"$event_path"
printf 'compile_error!("untrusted head checker must not execute");\n' >"$test_tool/src/main.rs"
printf 'compile_error!("head-only checker source must not execute");\n' >"$test_tool/src/untrusted.rs"
git -C "$test_repository" -c core.autocrlf=false -c user.name=LocalHold -c user.email=localhold@example.invalid add \
    tools/maintainability/src/main.rs tools/maintainability/src/untrusted.rs
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'untrusted checker head'
test_head=$(git -C "$test_repository" rev-parse HEAD)
if GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=0000000000000000000000000000000000000000 \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted a checker revision other than GITHUB_SHA\n' >&2
    exit 1
fi
if GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_head run_local_check --test-environment >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted an untrusted checker base\n' >&2
    exit 1
fi
GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null
GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base "$check" --root "$test_repository" --test-environment >/dev/null

gate_candidate="$fixture/gate-candidate"
git -c core.autocrlf=false clone -q --no-hardlinks "$test_repository" "$gate_candidate"
candidate_gate_marker="$fixture/candidate-gate-ran"
printf '%s\n' '#!/usr/bin/bash' 'touch "$CANDIDATE_GATE_MARKER"' >"$gate_candidate/script/run-maintainability-gate.sh"
git -C "$gate_candidate" -c core.autocrlf=false -c user.name=LocalHold -c user.email=localhold@example.invalid add script/run-maintainability-gate.sh
git -C "$gate_candidate" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'untrusted candidate gate'
gate_candidate_head=$(git -C "$gate_candidate" rev-parse HEAD)
GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$gate_candidate_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base CANDIDATE_GATE_MARKER=$candidate_gate_marker \
    "$check" --root "$gate_candidate" --test-environment >/dev/null
if [[ -e $candidate_gate_marker ]]; then
    printf 'protected maintainability workflow executed a candidate gate script\n' >&2
    exit 1
fi

printf 'pub fn second_pushed_commit() {}\n' >"$test_repository/src/lib.rs"
git -C "$test_repository" -c core.autocrlf=false -c user.name=LocalHold -c user.email=localhold@example.invalid add src/lib.rs
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'second pushed commit'
test_push_head=$(git -C "$test_repository" rev-parse HEAD)
GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_push_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null

mkdir -p "$test_repository/tools/untrusted/src"
printf '%s\n' '[package]' 'name = "untrusted"' 'version = "0.1.0"' 'edition = "2024"' >"$test_repository/tools/untrusted/Cargo.toml"
printf 'pub fn untrusted() {}\n' >"$test_repository/tools/untrusted/src/lib.rs"
printf '%s\n' \
    'fn main() {' \
    '    std::fs::write(std::env::var("UNTRUSTED_BUILD_MARKER").unwrap(), "executed").unwrap();' \
    '}' >"$test_repository/tools/untrusted/build.rs"
printf '\nuntrusted = { path = "../untrusted" }\n' >>"$test_tool/Cargo.toml"
sed -i '/^name = "localhold-maintainability"$/,/^]$/ { /^dependencies = \[$/a\ "untrusted",
}' "$test_tool/Cargo.lock"
printf '\n[[package]]\nname = "untrusted"\nversion = "0.1.0"\n' >>"$test_tool/Cargo.lock"
untrusted_manifest_sha256=$(sha256_stream <"$test_tool/Cargo.toml")
untrusted_manifest_sha256=${untrusted_manifest_sha256%%[[:space:]]*}
untrusted_lockfile_sha256=$(sha256_stream <"$test_tool/Cargo.lock")
untrusted_lockfile_sha256=${untrusted_lockfile_sha256%%[[:space:]]*}
sed -i "s/^readonly reviewed_manifest_sha256=.*/readonly reviewed_manifest_sha256=$untrusted_manifest_sha256/" \
    "$test_repository/script/check-maintainability-bootstrap.sh"
sed -i "s/^readonly reviewed_lockfile_sha256=.*/readonly reviewed_lockfile_sha256=$untrusted_lockfile_sha256/" \
    "$test_repository/script/check-maintainability-bootstrap.sh"
git -C "$test_repository" -c core.autocrlf=false -c user.name=LocalHold -c user.email=localhold@example.invalid add \
    script/check-maintainability-bootstrap.sh tools/maintainability/Cargo.lock tools/maintainability/Cargo.toml tools/untrusted
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'untrusted checker dependency graph'
test_graph_head=$(git -C "$test_repository" rev-parse HEAD)
untrusted_build_marker="$fixture/untrusted-build-ran"
if ! GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_graph_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base UNTRUSTED_BUILD_MARKER=$untrusted_build_marker \
    run_local_check --test-environment >/dev/null; then
    printf 'maintainability bootstrap did not preserve the trusted base checker dependency graph\n' >&2
    exit 1
fi
if [[ -e $untrusted_build_marker ]]; then
    printf 'maintainability bootstrap executed an untrusted checker dependency build script\n' >&2
    exit 1
fi

git -C "$test_repository" show "$test_base:tools/maintainability/Cargo.toml" >"$test_tool/Cargo.toml"
trusted_manifest_sha256=$(sha256_stream <"$test_tool/Cargo.toml")
trusted_manifest_sha256=${trusted_manifest_sha256%%[[:space:]]*}
sed -i "s/^readonly reviewed_manifest_sha256=.*/readonly reviewed_manifest_sha256=$trusted_manifest_sha256/" \
    "$test_repository/script/check-maintainability-bootstrap.sh"
git -C "$test_repository" -c core.autocrlf=false -c user.name=LocalHold -c user.email=localhold@example.invalid add \
    script/check-maintainability-bootstrap.sh tools/maintainability/Cargo.toml
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'untrusted checker lock graph'
test_lock_head=$(git -C "$test_repository" rev-parse HEAD)
if ! GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_lock_head \
    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null; then
    printf 'maintainability bootstrap did not preserve the trusted base checker lock graph\n' >&2
    exit 1
fi
git -C "$test_repository" checkout -q --detach "$test_base"
restore_reviewed_graph

touch "$test_tool/build.rs"
expect_failure
rm "$test_tool/build.rs"

write_manifest $'[package]\nname = "checker"'
expect_failure

write_manifest $'[package]\nname = "checker"\nbuild = true'
expect_failure

restore_reviewed_graph
rm "$test_tool/Cargo.lock"
expect_failure

restore_reviewed_graph
printf '\n[dependencies.untrusted]\npath = "../untrusted"\n' >>"$test_tool/Cargo.toml"
mkdir -p "$test_repository/tools/untrusted"
touch "$test_repository/tools/untrusted/build.rs"
expect_failure_before_command

restore_reviewed_graph
printf '\n# unreviewed lock graph\n' >>"$test_tool/Cargo.lock"
expect_failure

restore_reviewed_graph
printf '\nmaintainability:\n    true\n' >>"$test_repository/Justfile"
expect_failure_before_command

restore_reviewed_graph
printf '\n_.path = ["{{config_root}}/bin"]\n' >>"$test_repository/mise.toml"
expect_failure_before_command

restore_reviewed_graph
printf '\n# bypass reviewed runner\n' >>"$test_repository/script/run-source-safety.sh"
expect_failure_before_command

restore_reviewed_graph
mkdir -p "$test_repository/.cargo"
touch "$test_repository/.cargo/config.toml"
expect_failure
rm -r "$test_repository/.cargo"

mkdir -p "$fixture/.cargo"
touch "$fixture/.cargo/config"
expect_failure
rm -r "$fixture/.cargo"

cargo_home="$fixture/cargo-home"
mkdir -p "$cargo_home"
touch "$cargo_home/config.toml"
export CARGO_HOME=$cargo_home
expect_failure
unset CARGO_HOME
rm -r "$cargo_home"

restore_reviewed_graph
if run_check -- just maintainability >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted the removed arbitrary-command interface\n' >&2
    exit 1
fi
if run_check --source-safety >/dev/null 2>&1; then
    printf 'maintainability bootstrap allowed an alternate root to run a delegated gate\n' >&2
    exit 1
fi

inherited_cargo_function() {
    :
}
export -f inherited_cargo_function
if run_check --test-environment >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted an inherited exported shell function\n' >&2
    exit 1
fi
unset -f inherited_cargo_function

fake_bin="$fixture/fake-bin"
mkdir "$fake_bin"
cargo_name=cargo
printf '%s\n' '#!/usr/bin/bash' 'touch "$FAKE_JUST_MARKER"' >"$fake_bin/just"
printf '%s\n' '#!/usr/bin/bash' 'touch "$FAKE_CARGO_MARKER"' >"$fake_bin/$cargo_name"
printf '%s\n' '#!/usr/bin/bash' 'touch "$FAKE_RUSTUP_MARKER"' >"$fake_bin/rustup"
chmod +x "$fake_bin/just" "$fake_bin/$cargo_name" "$fake_bin/rustup"
fake_just_marker="$fixture/fake-just-ran"
fake_cargo_marker="$fixture/fake-cargo-ran"
fake_rustup_marker="$fixture/fake-rustup-ran"
FAKE_JUST_MARKER=$fake_just_marker FAKE_CARGO_MARKER=$fake_cargo_marker FAKE_RUSTUP_MARKER=$fake_rustup_marker PATH="$fake_bin:$PATH" run_check --test-environment >/dev/null
if [[ -e $fake_just_marker || -e $fake_cargo_marker || -e $fake_rustup_marker ]]; then
    printf 'maintainability bootstrap executed an untrusted PATH dispatcher\n' >&2
    exit 1
fi
if LOCALHOLD_MAINTAINABILITY_RUSTUP="$fake_bin/rustup" run_check --test-environment >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted an unauthenticated Rustup handoff\n' >&2
    exit 1
fi

fake_system_bin="$fixture/fake-system-bin"
mkdir "$fake_system_bin"
printf '%s\n' '#!/usr/bin/bash' 'exit 0' >"$fake_system_bin/git"
chmod +x "$fake_system_bin/git"
if PATH="$fake_system_bin:$PATH" run_check >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted a non-system Git executable\n' >&2
    exit 1
fi

tar_options_helper="$fixture/tar-options-helper"
tar_options_marker="$fixture/tar-options-ran"
printf '%s\n' '#!/usr/bin/bash' 'printf executed >"$TAR_OPTIONS_MARKER"' >"$tar_options_helper"
chmod +x "$tar_options_helper"
TAR_OPTIONS_MARKER=$tar_options_marker TAR_OPTIONS="--checkpoint=1 --checkpoint-action=exec=$tar_options_helper" run_check --test-environment >/dev/null
if [[ -e $tar_options_marker ]]; then
    printf 'maintainability bootstrap executed inherited TAR_OPTIONS\n' >&2
    exit 1
fi

kernel=$(/usr/bin/uname -s)
machine=$(/usr/bin/uname -m)
if [[ $kernel == Linux && $machine == x86_64 ]]; then
    toolchain_triple=x86_64-unknown-linux-gnu
    tool_extension=
    runtime_library_one=lib/librustc_driver-a39fc8bf9e61dbb9.so
    runtime_library_two=lib/libLLVM.so.22.1-rust-1.97.0-stable
elif [[ $kernel == MINGW* || $kernel == MSYS* || $kernel == CYGWIN* ]] && [[ $machine == x86_64 ]]; then
    toolchain_triple=x86_64-pc-windows-msvc
    tool_extension=.exe
    runtime_library_one=bin/std-24270508f4fdc995.dll
    runtime_library_two=bin/rustc_driver-4aa755545f2784f5.dll
else
    printf 'maintainability bootstrap tests require Linux or Windows x86_64\n' >&2
    exit 1
fi
trusted_rustup_environment=${RUSTUP_HOME:-${HOME:?}/.rustup}
trusted_rustup_home=$trusted_rustup_environment
if [[ $trusted_rustup_home =~ ^[[:alpha:]]:[/\\] ]]; then
    trusted_rustup_home=$(/usr/bin/cygpath -u "$trusted_rustup_home")
fi
trusted_rustup_command=${LOCALHOLD_MAINTAINABILITY_RUSTUP:-rustup}
trusted_cargo=$(RUSTUP_HOME=$trusted_rustup_environment "$trusted_rustup_command" which --toolchain 1.97.0 cargo)
if [[ $kernel != Linux ]]; then
    trusted_cargo=$(/usr/bin/cygpath -u "$trusted_cargo")
fi
trusted_toolchain_bin=${trusted_cargo%/*}
trusted_toolchain_root=${trusted_toolchain_bin%/*}
if [[ ! -d $trusted_toolchain_bin ]]; then
    printf 'maintainability bootstrap tests require the pinned Rust 1.97.0 toolchain\n' >&2
    exit 1
fi
fake_rustup_home="$fixture/fake-rustup"
fake_toolchain_root="$fake_rustup_home/toolchains/1.97.0-$toolchain_triple"
fake_toolchain_bin="$fake_toolchain_root/bin"
mkdir -p "$fake_toolchain_bin"
cp "$trusted_toolchain_bin/cargo$tool_extension" "$fake_toolchain_bin/cargo$tool_extension"
cp "$trusted_toolchain_bin/cargo$tool_extension" "$fake_toolchain_bin/rustc$tool_extension"
chmod +x "$fake_toolchain_bin/cargo$tool_extension" "$fake_toolchain_bin/rustc$tool_extension"
if RUSTUP_HOME=$fake_rustup_home run_check --test-environment >/dev/null 2>&1; then
    printf 'maintainability bootstrap trusted unauthenticated tools beside an authentic Cargo executable\n' >&2
    exit 1
fi
for tool in cargo rustc rustdoc cargo-clippy clippy-driver cargo-fmt rustfmt; do
    cp "$trusted_toolchain_bin/$tool$tool_extension" "$fake_toolchain_bin/$tool$tool_extension"
done
for runtime_library in "$runtime_library_one" "$runtime_library_two"; do
    mkdir -p "$fake_toolchain_root/${runtime_library%/*}"
    cp "$trusted_toolchain_root/$runtime_library" "$fake_toolchain_root/$runtime_library"
done
fake_rustup_environment=$fake_rustup_home
if [[ $kernel != Linux ]]; then
    fake_rustup_environment=$(/usr/bin/cygpath -w "$fake_rustup_home")
fi
RUSTUP_HOME=$fake_rustup_environment run_check --test-environment >/dev/null
printf '\nunauthenticated runtime\n' >>"$fake_toolchain_root/$runtime_library_one"
if RUSTUP_HOME=$fake_rustup_environment run_check --test-environment >/dev/null 2>&1; then
    printf 'maintainability bootstrap trusted an unauthenticated Rust runtime library\n' >&2
    exit 1
fi

bash_env=$fixture/bash-env
: >"$bash_env"
BASH_ENV=$bash_env GITHUB_PATH=untrusted LD_AUDIT='' LD_LIBRARY_PATH='' LD_PRELOAD='' RUSTDOCFLAGS=untrusted CARGO_ENCODED_RUSTFLAGS=untrusted CARGO_ENCODED_RUSTDOCFLAGS=untrusted RUSTC_BOOTSTRAP=untrusted CARGO_HOME="$fixture/untrusted-cargo-home" CARGO_TARGET_DIR="$fixture/untrusted-target" CLIPPY_CONF_DIR=untrusted GIT_DIR=untrusted \
    RUSTDOC=untrusted RUSTC_WRAPPER=untrusted CARGO_BUILD_RUSTDOC=untrusted CARGO_BUILD_RUSTDOCFLAGS=untrusted \
    CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_RUSTDOCFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \
    run_check --test-environment >/dev/null

printf 'maintainability bootstrap tests passed\n'
