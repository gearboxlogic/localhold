#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
check="$repository_root/script/check-maintainability-bootstrap.sh"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
test_repository="$fixture/parent/repository"
mkdir -p "$test_repository/tools/maintainability"

write_manifest() {
    printf '%s\n' "$1" >"$test_repository/tools/maintainability/Cargo.toml"
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

write_manifest $'[package]\nname = "checker"\nbuild = false'
run_check >/dev/null

touch "$test_repository/tools/maintainability/build.rs"
expect_failure
rm "$test_repository/tools/maintainability/build.rs"

write_manifest $'[package]\nname = "checker"'
expect_failure

write_manifest $'[package]\nname = "checker"\nbuild = true'
expect_failure

write_manifest $'[package]\nname = "checker"\nbuild = false'
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
