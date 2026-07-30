#!/usr/bin/bash
set -euo pipefail

unset GITHUB_ACTIONS GITHUB_EVENT_PATH GITHUB_SHA

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
check="$repository_root/script/check-maintainability-bootstrap.sh"
ci_workflow="$repository_root/.github/workflows/ci.yml"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
test_repository="$fixture/parent/repository"
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
if (( guard_count != 4 )); then
    printf 'every CI maintainability bootstrap execution must have an immediate workflow digest guard\n' >&2
    exit 1
fi
for loader_variable in LD_AUDIT LD_LIBRARY_PATH LD_PRELOAD; do
    loader_guard_count=$(grep -Fc "          $loader_variable: ''" "$ci_workflow" || true)
    if (( loader_guard_count != 4 )); then
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
    cp "$source_runner" "$test_repository/script/run-source-safety.sh"
    cp "$source_gate_runner" "$test_repository/script/run-maintainability-gate.sh"
    printf '%s\n' '#!/usr/bin/bash' 'printf reviewed-command\\n' >"$test_repository/script/reviewed-command.sh"
    mkdir -p "$test_repository/script/tests"
    cp "$source_bootstrap_tests" "$test_repository/script/tests/test_maintainability_bootstrap.sh"
    mkdir -p "$test_repository/src"
    printf 'pub fn reviewed() {}\n' >"$test_repository/src/lib.rs"
}

run_check() {
    "$check" --root "$test_repository" "$@"
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
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid add .
git -C "$test_repository" -c user.name=LocalHold -c user.email=localhold@example.invalid commit -qm 'reviewed fixture'
test_head=$(git -C "$test_repository" rev-parse HEAD)
run_check >/dev/null

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
if GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=0000000000000000000000000000000000000000 run_check >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted a checker revision other than GITHUB_SHA\n' >&2
    exit 1
fi
GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head run_check >/dev/null

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

mkdir -p "$fixture/parent/.cargo"
touch "$fixture/parent/.cargo/config"
expect_failure
rm -r "$fixture/parent/.cargo"

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

fake_system_bin="$fixture/fake-system-bin"
mkdir "$fake_system_bin"
printf '%s\n' '#!/usr/bin/bash' 'exit 0' >"$fake_system_bin/git"
chmod +x "$fake_system_bin/git"
if PATH="$fake_system_bin:$PATH" run_check >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted a non-system Git executable\n' >&2
    exit 1
fi

kernel=$(/usr/bin/uname -s)
machine=$(/usr/bin/uname -m)
if [[ $kernel == Linux && $machine == x86_64 ]]; then
    toolchain_triple=x86_64-unknown-linux-gnu
    tool_extension=
elif [[ $kernel == MINGW* || $kernel == MSYS* || $kernel == CYGWIN* ]] && [[ $machine == x86_64 ]]; then
    toolchain_triple=x86_64-pc-windows-msvc
    tool_extension=.exe
else
    printf 'maintainability bootstrap tests require Linux or Windows x86_64\n' >&2
    exit 1
fi
trusted_rustup_environment=${RUSTUP_HOME:-${HOME:?}/.rustup}
trusted_rustup_home=$trusted_rustup_environment
if [[ $trusted_rustup_home =~ ^[[:alpha:]]:[/\\] ]]; then
    trusted_rustup_home=$(/usr/bin/cygpath -u "$trusted_rustup_home")
fi
trusted_cargo=$(RUSTUP_HOME=$trusted_rustup_environment rustup which --toolchain 1.97.0 cargo)
if [[ $kernel != Linux ]]; then
    trusted_cargo=$(/usr/bin/cygpath -u "$trusted_cargo")
fi
trusted_toolchain_bin=${trusted_cargo%/*}
if [[ ! -d $trusted_toolchain_bin ]]; then
    printf 'maintainability bootstrap tests require the pinned Rust 1.97.0 toolchain\n' >&2
    exit 1
fi
fake_rustup_home="$fixture/fake-rustup"
fake_toolchain_bin="$fake_rustup_home/toolchains/1.97.0-$toolchain_triple/bin"
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
fake_rustup_environment=$fake_rustup_home
if [[ $kernel != Linux ]]; then
    fake_rustup_environment=$(/usr/bin/cygpath -w "$fake_rustup_home")
fi
RUSTUP_HOME=$fake_rustup_environment run_check --test-environment >/dev/null

bash_env=$fixture/bash-env
: >"$bash_env"
BASH_ENV=$bash_env LD_AUDIT='' LD_LIBRARY_PATH='' LD_PRELOAD='' RUSTDOCFLAGS=untrusted CARGO_ENCODED_RUSTFLAGS=untrusted CARGO_ENCODED_RUSTDOCFLAGS=untrusted RUSTC_BOOTSTRAP=untrusted CLIPPY_CONF_DIR=untrusted GIT_DIR=untrusted \
    RUSTDOC=untrusted RUSTC_WRAPPER=untrusted CARGO_BUILD_RUSTDOC=untrusted CARGO_BUILD_RUSTDOCFLAGS=untrusted \
    CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_RUSTDOCFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \
    run_check --test-environment >/dev/null

printf 'maintainability bootstrap tests passed\n'
