#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
check="$repository_root/script/check-maintainability-bootstrap.sh"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
test_repository="$fixture/parent/repository"
mkdir -p "$test_repository/tools/maintainability"
source_tool="$repository_root/tools/maintainability"
test_tool="$test_repository/tools/maintainability"

write_manifest() {
    printf '%s\n' "$1" >"$test_tool/Cargo.toml"
}

restore_reviewed_graph() {
    cp "$source_tool/Cargo.toml" "$test_tool/Cargo.toml"
    cp "$source_tool/Cargo.lock" "$test_tool/Cargo.lock"
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
    local marker="$fixture/command-ran"
    if run_check -- touch "$marker" >/dev/null 2>&1; then
        printf 'maintainability bootstrap fixture unexpectedly passed\n' >&2
        exit 1
    fi
    if [[ -e "$marker" ]]; then
        printf 'maintainability bootstrap executed a command for an unreviewed dependency graph\n' >&2
        exit 1
    fi
}

restore_reviewed_graph
run_check >/dev/null

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

RUSTC_WRAPPER=untrusted CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \
    run_check -- bash -c '[[ ! -v RUSTC_WRAPPER && ! -v CARGO_TARGET_TEST_RUSTFLAGS && ! -v CARGO_TARGET_TEST_LINKER && ! -v CARGO_TARGET_TEST_RUNNER ]]' >/dev/null

printf 'maintainability bootstrap tests passed\n'
