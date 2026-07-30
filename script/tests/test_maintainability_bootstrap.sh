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
source_runner="$repository_root/script/run-source-safety.sh"

write_manifest() {
    printf '%s\n' "$1" >"$test_tool/Cargo.toml"
}

restore_reviewed_graph() {
    cp "$source_tool/Cargo.toml" "$test_tool/Cargo.toml"
    cp "$source_tool/Cargo.lock" "$test_tool/Cargo.lock"
    cp "$repository_root/mise.toml" "$test_repository/mise.toml"
    cp "$repository_root/mise.lock" "$test_repository/mise.lock"
    mkdir -p "$test_repository/script"
    cp "$source_runner" "$test_repository/script/run-source-safety.sh"
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

fake_bin="$fixture/fake-bin"
mkdir "$fake_bin"
real_cygpath="$fake_bin/real-cygpath"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    '[[ ${1:-} == -u && ${2:-} == "C:\cargo-home" ]] || exit 1' \
    'printf "%s\n" "$FAKE_CARGO_HOME"' >"$real_cygpath"
chmod +x "$real_cygpath"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'if [[ ${1:-} == -m && ${2:-} == "$FAKE_DRIVE_ROOT" ]]; then' \
    '    printf "D:/\n"' \
    '    exit' \
    'fi' \
    '[[ -n ${REAL_CYGPATH:-} ]] || exit 1' \
    'exec "$REAL_CYGPATH" "$@"' >"$fake_bin/cygpath"
chmod +x "$fake_bin/cygpath"
mkdir -p "$fixture/parent/.cargo"
touch "$fixture/parent/.cargo/config"
fake_drive_root=$(cd -- "$test_repository" && pwd -P)
CARGO_HOME='C:\cargo-home' \
    FAKE_CARGO_HOME=$fixture \
    FAKE_DRIVE_ROOT=$fake_drive_root \
    REAL_CYGPATH=$real_cygpath \
    PATH="$fake_bin:$PATH" \
    run_check >/dev/null
rm -r "$fixture/parent/.cargo"

cargo_home="$fixture/cargo-home"
mkdir -p "$cargo_home"
touch "$cargo_home/config.toml"
export CARGO_HOME=$cargo_home
expect_failure
unset CARGO_HOME
rm -r "$cargo_home"

restore_reviewed_graph
mkdir -p "$test_repository/bin"
cargo_name=cargo
fake_cargo="$test_repository/bin/$cargo_name"
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' >"$fake_cargo"
chmod +x "$fake_cargo"
if PATH="$test_repository/bin:$PATH" run_check -- touch "$fixture/repository-cargo-ran" >/dev/null 2>&1; then
    printf 'maintainability bootstrap accepted a repository-controlled Cargo executable\n' >&2
    exit 1
fi
if [[ -e "$fixture/repository-cargo-ran" ]]; then
    printf 'maintainability bootstrap ran a command through repository-controlled Cargo\n' >&2
    exit 1
fi
rm -r "$test_repository/bin"

RUSTDOCFLAGS=untrusted CARGO_ENCODED_RUSTFLAGS=untrusted CARGO_ENCODED_RUSTDOCFLAGS=untrusted CLIPPY_CONF_DIR=untrusted \
    RUSTDOC=untrusted RUSTC_WRAPPER=untrusted CARGO_BUILD_RUSTDOC=untrusted CARGO_BUILD_RUSTDOCFLAGS=untrusted \
    CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_RUSTDOCFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \
    run_check -- bash -c 'for name in RUSTDOCFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS CLIPPY_CONF_DIR RUSTDOC RUSTC_WRAPPER CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTDOCFLAGS CARGO_TARGET_TEST_RUSTFLAGS CARGO_TARGET_TEST_RUSTDOCFLAGS CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do [[ ! -v $name ]] || exit 1; done' >/dev/null

printf 'maintainability bootstrap tests passed\n'
