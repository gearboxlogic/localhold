#!/usr/bin/bash
set -euo pipefail

readonly mode=${1:-}
if (( $# != 1 )) || [[ $mode != source-safety && $mode != dependency-unsafe && $mode != maintainability && $mode != test-environment ]]; then
    printf 'maintainability gate runner requires one fixed mode\n' >&2
    exit 1
fi

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly repository_root
cd -- "$repository_root"

readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}
readonly sha256_command=/usr/bin/sha256sum
readonly uname_command=/usr/bin/uname
readonly bash_command=/usr/bin/bash
for system_command in "$sha256_command" "$uname_command" "$bash_command"; do
    if [[ ! -f $system_command || ! -x $system_command ]]; then
        printf 'maintainability gate requires an OS-owned executable: %s\n' "$system_command" >&2
        exit 1
    fi
done

sha256_file() {
    local output
    output=$("$sha256_command" -- "$1")
    printf '%s\n' "${output%%[[:space:]]*}"
}

authenticated_tool() {
    local path=$1
    local expected=$2
    if [[ ! -f $path || -L $path || ! -x $path ]]; then
        printf 'pinned Rust tool must be a regular non-symlink executable: %s\n' "$path" >&2
        exit 1
    fi
    local actual
    actual=$(sha256_file "$path")
    if [[ $actual != "$expected" ]]; then
        printf 'reviewed Rust tool digest differs: %s\n' "$path" >&2
        exit 1
    fi
    printf '%s\n' "$path"
}

kernel=$("$uname_command" -s)
machine=$("$uname_command" -m)
# Tool digests come from the SHA-256-verified official Rust 1.97.0 component
# archives and the official Rustup 1.29.0 archive.
if [[ $kernel == Linux && $machine == x86_64 ]]; then
    tool_extension=
    expected_rustup_sha256=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
    expected_cargo_sha256=eff12bab37b9d9e01324db4583eaf55b2cd82ac3008a7e59876e4cd2e9a028f5
    expected_rustc_sha256=df13f58759c0662831983e3a6501c63c1fc12ea60ec4e1d1ac35e5fe43c500c0
    expected_rustdoc_sha256=ed74ac3f2be8270ed5da788cdecb8cd0e530fb6b5b380e63aedb62323beb7c85
    expected_cargo_clippy_sha256=54cdb363ee168217ad2b5306b242d53795b1e48c8faa8f239442e5020a4cbc58
    expected_clippy_driver_sha256=2230bc9dd3084c8032c5bbe7efef288e55715b5831e54a1dc3fe1182d1884584
    expected_cargo_fmt_sha256=024e11d5e200ab70d76a2c6f973784b2c92896cb65e2f4235c79f34ed4836233
    expected_rustfmt_sha256=fbdba7404a80bc6d36fa2bb4cdd7ca3fc7f060a109207ef03eaef63058cd1216
    windows_toolchain=false
elif [[ $kernel == MINGW* || $kernel == MSYS* || $kernel == CYGWIN* ]] && [[ $machine == x86_64 ]]; then
    tool_extension=.exe
    expected_rustup_sha256=86478e53f769379d7f0ebfa7c9aa97cb76ca92233f79aa2cc0dbee2efaac73c7
    expected_cargo_sha256=3cd119fe81dfedb9dce4573696bf65058f16b57c9e5babe415b71624315cbb7d
    expected_rustc_sha256=6d1c5543ed3a45cfbc1c1332d42d6550d883c14d3c2e323427e631c331cebeeb
    expected_rustdoc_sha256=7967c1da7e3bec5cd699b7f4bf7a69a1f9292e1ac38bef8dba403fa55ebddda3
    expected_cargo_clippy_sha256=d2e4a82ae44b78ab9b218d23ad9a9284f3e71efde5573edaf3076cbbf2b9213e
    expected_clippy_driver_sha256=4ff747c4e2a55d05bfd42b6ea1849c558d4533e44ef44e009706b2be473c5450
    expected_cargo_fmt_sha256=3eb9b94afb841b7dd95687a35fe815d592bc130961147213d4f2b5dd579597bd
    expected_rustfmt_sha256=607162816ec4f330fee34d6954e20631a76657c4264d75ae80c14183f8dc1b18
    windows_toolchain=true
else
    printf 'maintainability evidence requires Linux or Windows x86_64, not %s %s\n' "$kernel" "$machine" >&2
    exit 1
fi

if $windows_toolchain; then
    readonly cygpath_command=/usr/bin/cygpath
    [[ -f $cygpath_command && -x $cygpath_command ]] || {
        printf 'maintainability gate requires an OS-owned cygpath\n' >&2
        exit 1
    }
fi

rustup_home=${RUSTUP_HOME:-${HOME:?maintainability gate requires RUSTUP_HOME or HOME}/.rustup}
if [[ $rustup_home =~ ^[[:alpha:]]:[/\\] ]]; then
    rustup_home=$("$cygpath_command" -u "$rustup_home")
elif [[ $rustup_home != /* ]]; then
    printf 'maintainability gate requires an absolute Rustup home\n' >&2
    exit 1
fi
rustup_environment=$rustup_home
if $windows_toolchain; then
    rustup_environment=$("$cygpath_command" -w "$rustup_home")
fi

rustup_executable=${LOCALHOLD_MAINTAINABILITY_RUSTUP:-}
if [[ -n $rustup_executable && $rustup_executable != /* ]]; then
    printf 'maintainability gate requires an absolute authenticated Rustup handoff\n' >&2
    exit 1
fi
if [[ -z $rustup_executable ]]; then
    IFS=: read -r -a path_directories <<<"$PATH"
    for rustup_directory in "${path_directories[@]}"; do
        [[ -n $rustup_directory ]] || rustup_directory=.
        candidate="$rustup_directory/rustup$tool_extension"
        if [[ ! -f $candidate || -L $candidate || ! -x $candidate ]]; then
            continue
        fi
        rustup_directory=$(cd -- "$rustup_directory" && pwd -P)
        candidate="$rustup_directory/rustup$tool_extension"
        if [[ $(sha256_file "$candidate") == "$expected_rustup_sha256" ]]; then
            rustup_executable=$candidate
            break
        fi
    done
fi
if [[ -z $rustup_executable ]]; then
    printf 'maintainability gate requires Rustup 1.29.0 on PATH\n' >&2
    exit 1
fi
rustup_executable=$(authenticated_tool "$rustup_executable" "$expected_rustup_sha256")
LOCALHOLD_MAINTAINABILITY_RUSTUP=$rustup_executable
export LOCALHOLD_MAINTAINABILITY_RUSTUP

resolved_cargo=$(RUSTUP_HOME=$rustup_environment "$rustup_executable" which --toolchain 1.97.0 cargo) || {
    printf 'authenticated Rustup could not resolve the pinned Rust 1.97.0 Cargo executable\n' >&2
    exit 1
}
if [[ -z $resolved_cargo || $resolved_cargo == *$'\n'* ]]; then
    printf 'authenticated Rustup returned an invalid Cargo path\n' >&2
    exit 1
fi
if $windows_toolchain; then
    resolved_cargo=$("$cygpath_command" -u "$resolved_cargo")
elif [[ $resolved_cargo != /* ]]; then
    printf 'authenticated Rustup returned a non-absolute Cargo path\n' >&2
    exit 1
fi
toolchain_bin=${resolved_cargo%/*}
if [[ ! -d $toolchain_bin || -L $toolchain_bin ]]; then
    printf 'resolved pinned Rust toolchain bin must be a regular directory: %s\n' "$toolchain_bin" >&2
    exit 1
fi
toolchain_bin=$(cd -- "$toolchain_bin" && pwd -P)

cargo_executable=$(authenticated_tool "$toolchain_bin/cargo$tool_extension" "$expected_cargo_sha256")
rustc_executable=$(authenticated_tool "$toolchain_bin/rustc$tool_extension" "$expected_rustc_sha256")
rustdoc_executable=$(authenticated_tool "$toolchain_bin/rustdoc$tool_extension" "$expected_rustdoc_sha256")
cargo_clippy_executable=$(authenticated_tool "$toolchain_bin/cargo-clippy$tool_extension" "$expected_cargo_clippy_sha256")
authenticated_tool "$toolchain_bin/clippy-driver$tool_extension" "$expected_clippy_driver_sha256" >/dev/null
cargo_fmt_executable=$(authenticated_tool "$toolchain_bin/cargo-fmt$tool_extension" "$expected_cargo_fmt_sha256")
rustfmt_executable=$(authenticated_tool "$toolchain_bin/rustfmt$tool_extension" "$expected_rustfmt_sha256")

native_cargo=$cargo_executable
native_rustc=$rustc_executable
native_rustdoc=$rustdoc_executable
native_rustfmt=$rustfmt_executable
if $windows_toolchain; then
    native_cargo=$("$cygpath_command" -w "$native_cargo")
    native_rustc=$("$cygpath_command" -w "$native_rustc")
    native_rustdoc=$("$cygpath_command" -w "$native_rustdoc")
    native_rustfmt=$("$cygpath_command" -w "$native_rustfmt")
fi
trusted_path="$toolchain_bin:/usr/bin:/bin"
if $windows_toolchain; then
    readonly vswhere_command="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    if [[ ! -f $vswhere_command || -L $vswhere_command || ! -x $vswhere_command ]]; then
        printf 'maintainability gate requires the Visual Studio-owned installation locator\n' >&2
        exit 1
    fi
    linker_candidates=$("$vswhere_command" -nologo -latest -products "*" -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\link.exe') || {
        printf 'maintainability gate could not locate the MSVC linker\n' >&2
        exit 1
    }
    trusted_linker_bin=
    while IFS= read -r linker_candidate; do
        linker_candidate=${linker_candidate%$'\r'}
        [[ -n $linker_candidate ]] || continue
        linker_candidate=$("$cygpath_command" -u "$linker_candidate")
        if [[ ! -f $linker_candidate || -L $linker_candidate || ! -x $linker_candidate ]]; then
            continue
        fi
        linker_name=${linker_candidate##*/}
        linker_directory=${linker_candidate%/*}
        linker_directory=$(cd -- "$linker_directory" && pwd -P)
        linker_candidate="$linker_directory/$linker_name"
        case ${linker_directory,,} in
            /[a-z]/program\ files/microsoft\ visual\ studio/*/vc/tools/msvc/*/bin/hostx64/x64)
                if [[ ${linker_name,,} == link.exe && -f $linker_candidate && ! -L $linker_candidate && -x $linker_candidate ]]; then
                    trusted_linker_bin=$linker_directory
                    break
                fi
                ;;
        esac
    done <<<"$linker_candidates"
    if [[ -z $trusted_linker_bin ]]; then
        printf 'maintainability gate requires an OS-owned Visual Studio Hostx64 linker\n' >&2
        exit 1
    fi
    trusted_path="$toolchain_bin:$trusted_linker_bin:/usr/bin:/mingw64/bin:/c/Windows/System32"
fi
PATH=$trusted_path
readonly PATH
CARGO=$native_cargo
RUSTC=$native_rustc
RUSTDOC=$native_rustdoc
RUSTFMT=$native_rustfmt
LOCALHOLD_MAINTAINABILITY_CARGO=$native_cargo
export PATH CARGO RUSTC RUSTDOC RUSTFMT LOCALHOLD_MAINTAINABILITY_CARGO LOCALHOLD_MAINTAINABILITY_RUSTUP

run_source_safety() {
    "$bash_command" "$repository_root/script/tests/test_maintainability_bootstrap.sh"
    "$bash_command" "$repository_root/script/run-source-safety.sh"
}

run_dependency_unsafe() {
    "$cargo_executable" fetch --locked
    "$cargo_executable" fetch --manifest-path tools/dependency-unsafe/Cargo.toml --locked
    "$cargo_fmt_executable" fmt --manifest-path tools/dependency-unsafe/Cargo.toml -- --check
    "$cargo_executable" test --manifest-path tools/dependency-unsafe/Cargo.toml --locked
    "$cargo_clippy_executable" clippy --manifest-path tools/dependency-unsafe/Cargo.toml --all-targets --locked -- -D warnings
    "$cargo_executable" run --manifest-path tools/dependency-unsafe/Cargo.toml --locked -- check
}

verify_test_environment() {
    local name
    for name in BASH_ENV GITHUB_PATH LD_AUDIT LD_LIBRARY_PATH LD_PRELOAD RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS RUSTC_BOOTSTRAP CLIPPY_CONF_DIR GIT_DIR RUSTC_WRAPPER \
        RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTDOCFLAGS CARGO_TARGET_TEST_RUSTFLAGS \
        CARGO_TARGET_TEST_RUSTDOCFLAGS CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do
        if [[ -v $name ]]; then
            printf 'maintainability bootstrap retained an untrusted environment channel: %s\n' "$name" >&2
            exit 1
        fi
    done
    [[ -n $LOCALHOLD_MAINTAINABILITY_CARGO && -n $LOCALHOLD_MAINTAINABILITY_RUSTUP && -n $git_command ]]
    if [[ $PATH != "$trusted_path" ]]; then
        printf 'maintainability bootstrap retained an untrusted executable search path\n' >&2
        exit 1
    fi
    printf 'maintainability bootstrap environment test passed\n'
}

case "$mode" in
    source-safety) run_source_safety ;;
    dependency-unsafe) run_dependency_unsafe ;;
    maintainability)
        run_source_safety
        run_dependency_unsafe
        ;;
    test-environment) verify_test_environment ;;
esac
