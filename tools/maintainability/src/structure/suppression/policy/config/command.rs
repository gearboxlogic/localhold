use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

mod actions;
mod arguments;
mod environment;
mod make;
mod surfaces;
mod yaml;
use super::cargo::tracked_manifests;
pub(super) use arguments::has_sourced_file_indirection;
#[cfg(test)]
pub(super) use arguments::weakening_token;
pub(super) use arguments::weakening_token_for_surface;
use arguments::{
    cargo_manifest_paths_for_surface, direct_rust_sources_for_surface, normalized_shell_tokens, normalized_shell_words, package_script_commands,
    weakening_token_in_reviewed_shell_remainder,
};
use environment::{is_weakening_environment_assignment_name, is_weakening_environment_name};
use surfaces::execution_surfaces;

pub(super) const BOOTSTRAP_ENVIRONMENT_LINES: &[&str] = &[
    "if /usr/bin/env | /usr/bin/grep '^BASH_FUNC_' >/dev/null; then",
    "    /usr/bin/false",
    "cargo_home=${CARGO_HOME:-}",
    "        cargo_home=\"$HOME/.cargo\"",
    "        cargo_home=\"$USERPROFILE/.cargo\"",
    "    cargo_home=\"$repository_root/$cargo_home\"",
    "    cargo_home=$(\"$cygpath_command\" -u \"$cargo_home\")",
    "git_command=$(trusted_system_command git)",
    "GIT_CONFIG_NOSYSTEM=1",
    "GIT_CONFIG_GLOBAL=/dev/null",
    "readonly GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL",
    "export GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL",
    "    local configured_base=${LOCALHOLD_MAINTAINABILITY_BASE_REV:-}",
    "    git_executable=$git_command",
    "        git_executable=$(\"$cygpath_command\" -w \"$git_executable\")",
    "    LOCALHOLD_MAINTAINABILITY_GIT=$git_executable",
    "    export LOCALHOLD_MAINTAINABILITY_GIT",
    "            BASH_ENV | GITHUB_PATH | LD_AUDIT | LD_LIBRARY_PATH | LD_PRELOAD | RUSTFLAGS | RUSTDOCFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_ENCODED_RUSTDOCFLAGS | RUSTC_BOOTSTRAP | CARGO_BUILD_TARGET | CARGO_TARGET_DIR | CLIPPY_ARGS | CLIPPY_CONF_DIR | \\",
    "                RUSTC | RUSTDOC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTDOC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | \\",
    "                CARGO_BUILD_RUSTFLAGS | CARGO_BUILD_RUSTDOCFLAGS | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | CARGO_TARGET_*_RUSTDOCFLAGS | \\",
    "                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER | GIT_* | LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT | TAR_OPTIONS)",
    "if [[ -v GITHUB_ACTIONS || -v GITHUB_EVENT_PATH || -v GITHUB_SHA ]]; then",
    "        if [[ ${GITHUB_ACTIONS:-} != true || -z ${GITHUB_EVENT_PATH:-} || -z ${GITHUB_SHA:-} ]]; then",
    "        if [[ ! $GITHUB_SHA =~ ^[[:xdigit:]]{40}$ || ${checked_head,,} != \"${GITHUB_SHA,,}\" ]]; then",
    "            printf 'checked-out Git head revision differs from GITHUB_SHA before checker compilation\\n' >&2",
    "        LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT=$snapshot_root \"$bash_command\" \"$snapshot_gate_runner\" \"$mode\" || status=$?",
];
pub(super) const GATE_RUNNER_ENVIRONMENT_LINES: &[&str] = &[
    "repository_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}",
    "LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT=$repository_root",
    "readonly implementation_root repository_root LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT",
    "export LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT",
    "readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}",
    "GIT_CONFIG_NOSYSTEM=1",
    "GIT_CONFIG_GLOBAL=/dev/null",
    "readonly GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL",
    "export GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL",
    "rustup_home=${RUSTUP_HOME:-${HOME:?maintainability gate requires RUSTUP_HOME or HOME}/.rustup}",
    "    rustup_home=$(\"$cygpath_command\" -u \"$rustup_home\")",
    "rustup_executable=${LOCALHOLD_MAINTAINABILITY_RUSTUP:-}",
    "LOCALHOLD_MAINTAINABILITY_RUSTUP=$rustup_executable",
    "export LOCALHOLD_MAINTAINABILITY_RUSTUP",
    "resolved_cargo=$(RUSTUP_HOME=$rustup_environment \"$rustup_executable\" which --toolchain 1.97.0 cargo) || {",
    "trusted_path=\"/usr/bin:/bin\"",
    "    trusted_path=\"$trusted_linker_bin:/usr/bin:/mingw64/bin:/c/Windows/System32\"",
    "compatibility_bin=\"$target_directory/b\"",
    "compatibility_rustc=\"$compatibility_bin/$rustc_name\"",
    "compatibility_cargo_clippy=\"$compatibility_bin/$cargo_clippy_name\"",
    "compatibility_clippy_driver=\"$compatibility_bin/$clippy_driver_name\"",
    "compatibility_rustc=$(authenticated_tool \"$compatibility_rustc\" \"$expected_rustup_sha256\")",
    "compatibility_cargo_clippy=$(authenticated_tool \"$compatibility_cargo_clippy\" \"$expected_rustup_sha256\")",
    "compatibility_clippy_driver=$(authenticated_tool \"$compatibility_clippy_driver\" \"$expected_rustup_sha256\")",
    "trusted_path=\"$compatibility_bin:$trusted_path\"",
    "PATH=$trusted_path",
    "readonly PATH",
    "RUSTUP_HOME=$rustup_environment",
    "RUSTUP_TOOLCHAIN=1.97.0",
    "readonly RUSTUP_HOME RUSTUP_TOOLCHAIN",
    "CARGO=$native_cargo",
    "RUSTC=$native_rustc",
    "RUSTDOC=$native_rustdoc",
    "LOCALHOLD_MAINTAINABILITY_CARGO=$native_cargo",
    "LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY=$native_cargo_clippy",
    "LOCALHOLD_MAINTAINABILITY_CARGO_FMT=$native_cargo_fmt",
    "LOCALHOLD_MAINTAINABILITY_RUSTC=$native_rustc",
    "CARGO_HOME=$native_cargo_home",
    "CARGO_TARGET_DIR=$relative_target_directory",
    "readonly CARGO_HOME CARGO_TARGET_DIR",
    "export PATH CARGO RUSTC RUSTDOC RUSTFMT RUSTUP_HOME RUSTUP_TOOLCHAIN CARGO_HOME CARGO_TARGET_DIR GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL LOCALHOLD_MAINTAINABILITY_CARGO LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY LOCALHOLD_MAINTAINABILITY_CARGO_FMT LOCALHOLD_MAINTAINABILITY_RUSTC LOCALHOLD_MAINTAINABILITY_RUSTUP",
    "    for name in BASH_ENV GITHUB_PATH LD_AUDIT LD_LIBRARY_PATH LD_PRELOAD RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS RUSTC_BOOTSTRAP CLIPPY_CONF_DIR GIT_DIR RUSTC_WRAPPER \\",
    "        RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTDOCFLAGS CARGO_TARGET_TEST_RUSTFLAGS \\",
    "        CARGO_TARGET_TEST_RUSTDOCFLAGS CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do",
    "    [[ -n $LOCALHOLD_MAINTAINABILITY_CARGO && -n $LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY && -n $LOCALHOLD_MAINTAINABILITY_CARGO_FMT && -n $LOCALHOLD_MAINTAINABILITY_RUSTC && -n $LOCALHOLD_MAINTAINABILITY_RUSTUP && -n $git_command ]]",
    "    if [[ ! -d $target_directory || -L $target_directory || ${target_directory%/*} != \"$target_parent\" || $CARGO_TARGET_DIR != \"$relative_target_directory\" || \"$repository_root/$CARGO_TARGET_DIR\" != \"$target_directory\" ]]; then",
    "    if [[ ! -d $fresh_cargo_home || -L $fresh_cargo_home || ${fresh_cargo_home%/*} != \"$target_directory\" || $CARGO_HOME != \"$native_cargo_home\" ]]; then",
    "    if [[ ! -d $compatibility_bin || -L $compatibility_bin || ${compatibility_bin%/*} != \"$target_directory\" ]]; then",
    "    if [[ $RUSTUP_HOME != \"$rustup_environment\" || $RUSTUP_TOOLCHAIN != 1.97.0 ]]; then",
    "    if [[ $GIT_CONFIG_NOSYSTEM != 1 || $GIT_CONFIG_GLOBAL != /dev/null ]]; then",
    "    if [[ $LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT != \"$repository_root\" ]]; then",
];
pub(super) const GATE_RUNNER_COMMAND_LINES: &[&str] = &[
    "    \"$cargo_executable\" fetch --locked",
    "    \"$cargo_executable\" fetch --manifest-path \"$audit_manifest\" --locked",
    "    \"$cargo_fmt_executable\" --manifest-path \"$audit_manifest\" -- --check",
    "    \"$cargo_executable\" test --manifest-path \"$audit_manifest\" --locked",
    "    \"$cargo_clippy_executable\" clippy --manifest-path \"$audit_manifest\" --all-targets --locked -- -D warnings",
    "    \"$cargo_executable\" run --manifest-path \"$audit_manifest\" --locked -- check",
];
pub(super) const RUNNER_ENVIRONMENT_LINES: &[&str] = &[
    "audit_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}",
    "readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}",
    "readonly cargo_clippy_command=${LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY:?maintainability bootstrap did not provide an absolute Cargo Clippy command}",
    "readonly cargo_fmt_command=${LOCALHOLD_MAINTAINABILITY_CARGO_FMT:?maintainability bootstrap did not provide an absolute Cargo fmt command}",
    "readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}",
];
pub(super) const RUNNER_COMMAND_LINES: &[&str] = &[
    "\"$cargo_command\" fetch --manifest-path \"$maintainability_manifest\" --locked",
    "\"$cargo_fmt_command\" --manifest-path \"$maintainability_manifest\" -- --check",
    "\"$cargo_command\" test --manifest-path \"$maintainability_manifest\" --locked",
    "\"$cargo_clippy_command\" clippy --manifest-path \"$maintainability_manifest\" --all-targets --locked -- -D warnings",
    "\"$cargo_command\" run --manifest-path \"$maintainability_manifest\" --locked -- check",
];
pub(super) const BOOTSTRAP_TEST_ENVIRONMENT_LINES: &[&str] = &[
    "unset GITHUB_ACTIONS GITHUB_EVENT_PATH GITHUB_SHA LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT LOCALHOLD_MAINTAINABILITY_BASE_REV",
    "fixture_parent=\"$repository_root/target/bootstrap-tests\"",
    "workflow_sha256=$(sed -n 's/^[[:space:]]*LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256: //p' \"$ci_workflow\")",
    "guard_count=$(grep -Fc 'if [[ \"$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256\" != \"$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256\" ]]; then' \"$ci_workflow\" || true)",
    "if GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=0000000000000000000000000000000000000000 \\",
    "    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null 2>&1; then",
    "    printf 'maintainability bootstrap accepted a checker revision other than GITHUB_SHA\\n' >&2",
    "if GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head \\",
    "    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_head run_local_check --test-environment >/dev/null 2>&1; then",
    "GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head \\",
    "    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null",
    "GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_push_head \\",
    "    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null",
    "if ! GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_graph_head \\",
    "    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base UNTRUSTED_BUILD_MARKER=$untrusted_build_marker \\",
    "    run_local_check --test-environment >/dev/null; then",
    "if ! GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_lock_head \\",
    "    LOCALHOLD_MAINTAINABILITY_BASE_REV=$test_base run_local_check --test-environment >/dev/null; then",
    "bash_env=$fixture/bash-env",
    "cargo_home=\"$fixture/cargo-home\"",
    "export CARGO_HOME=$cargo_home",
    "unset CARGO_HOME",
    "for loader_variable in LD_AUDIT LD_LIBRARY_PATH LD_PRELOAD; do",
    "FAKE_JUST_MARKER=$fake_just_marker FAKE_CARGO_MARKER=$fake_cargo_marker FAKE_RUSTUP_MARKER=$fake_rustup_marker PATH=\"$fake_bin:$PATH\" run_check --test-environment >/dev/null",
    "if LOCALHOLD_MAINTAINABILITY_RUSTUP=\"$fake_bin/rustup\" run_check --test-environment >/dev/null 2>&1; then",
    "if PATH=\"$fake_system_bin:$PATH\" run_check >/dev/null 2>&1; then",
    "TAR_OPTIONS_MARKER=$tar_options_marker TAR_OPTIONS=\"--checkpoint=1 --checkpoint-action=exec=$tar_options_helper\" run_check --test-environment >/dev/null",
    "    printf 'maintainability bootstrap executed inherited TAR_OPTIONS\\n' >&2",
    "export -f inherited_cargo_function",
    "if run_check --test-environment >/dev/null 2>&1; then",
    "unset -f inherited_cargo_function",
    "trusted_rustup_command=${LOCALHOLD_MAINTAINABILITY_RUSTUP:-rustup}",
    "trusted_rustup_environment=${RUSTUP_HOME:-${HOME:?}/.rustup}",
    "trusted_cargo=$(RUSTUP_HOME=$trusted_rustup_environment \"$trusted_rustup_command\" which --toolchain 1.97.0 cargo)",
    "if RUSTUP_HOME=$fake_rustup_home run_check --test-environment >/dev/null 2>&1; then",
    "RUSTUP_HOME=$fake_rustup_environment run_check --test-environment >/dev/null",
    "if RUSTUP_HOME=$fake_rustup_environment run_check --test-environment >/dev/null 2>&1; then",
    "BASH_ENV=$bash_env GITHUB_PATH=untrusted LD_AUDIT='' LD_LIBRARY_PATH='' LD_PRELOAD='' RUSTDOCFLAGS=untrusted CARGO_ENCODED_RUSTFLAGS=untrusted CARGO_ENCODED_RUSTDOCFLAGS=untrusted RUSTC_BOOTSTRAP=untrusted CARGO_HOME=\"$fixture/untrusted-cargo-home\" CARGO_TARGET_DIR=\"$fixture/untrusted-target\" CLIPPY_CONF_DIR=untrusted GIT_DIR=untrusted \\",
    "    RUSTDOC=untrusted RUSTC_WRAPPER=untrusted CARGO_BUILD_RUSTDOC=untrusted CARGO_BUILD_RUSTDOCFLAGS=untrusted \\",
    "    CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_RUSTDOCFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \\",
    "    run_check --test-environment >/dev/null",
];
pub(super) const MISE_ENVIRONMENT_LINES: &[&str] = &[
    "CARGO_HOME = \"{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \\\"/.cache\\\") }}/localhold/cargo\"",
    "RUSTUP_HOME = \"{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \\\"/.cache\\\") }}/localhold/rustup\"",
];
pub(super) const CI_TRUST_ENVIRONMENT_LINES: &[&str] = &[
    "        shell: /usr/bin/env -u BASH_ENV -u ENV -u SHELLOPTS -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}",
    "        shell: 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"$ErrorActionPreference = ''Stop''; $env:BASH_ENV = $null; $env:ENV = $null; $env:SHELLOPTS = $null; $env:LD_AUDIT = $null; $env:LD_LIBRARY_PATH = $null; $env:LD_PRELOAD = $null; & ''C:\\Program Files\\Git\\bin\\bash.exe'' --noprofile --norc -e -o pipefail ''{0}''; exit $LASTEXITCODE\"'",
    "          BASH_ENV: ''",
    "          BASH_ENV: ''",
    "          SHELLOPTS: ''",
    "          SHELLOPTS: ''",
    "          GIT_CONFIG_COUNT: '1'",
    "          GIT_CONFIG_KEY_0: core.autocrlf",
    "          GIT_CONFIG_VALUE_0: 'false'",
    "          LD_AUDIT: ''",
    "          LD_AUDIT: ''",
    "          LD_LIBRARY_PATH: ''",
    "          LD_LIBRARY_PATH: ''",
    "          LD_PRELOAD: ''",
    "          LD_PRELOAD: ''",
    "          RUSTUP_DIST_SERVER: https://static.rust-lang.org",
    "          RUSTUP_DIST_SERVER: https://static.rust-lang.org",
    "          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup",
    "          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup",
    "          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup",
    "          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup",
    "          RUSTUP_UPDATE_ROOT: https://static.rust-lang.org/rustup",
    "          RUSTUP_UPDATE_ROOT: https://static.rust-lang.org/rustup",
    "  LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256: 5f14d1bc87a4ebf78198919cf003770a9e5cc0fdc0a1be6f0e719822ff00f890",
    "          LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256: ${{ hashFiles('script/check-maintainability-bootstrap.sh') }}",
    "          LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256: ${{ hashFiles('script/check-maintainability-bootstrap.sh') }}",
    "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
    "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
    "          if [[ \"$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256\" != \"$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256\" ]]; then",
    "          if [[ \"$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256\" != \"$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256\" ]]; then",
];
pub(super) const TRUSTED_GATE_ENVIRONMENT_LINES: &[&str] = &[
    "        shell: /usr/bin/env -u BASH_ENV -u ENV -u SHELLOPTS -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}",
    "          RUSTUP_DIST_SERVER: https://static.rust-lang.org",
    "          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup",
    "          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup",
    "          RUSTUP_TOOLCHAIN: 1.97.0",
    "          RUSTUP_UPDATE_ROOT: https://static.rust-lang.org/rustup",
    "          export LOCALHOLD_MAINTAINABILITY_BASE_REV=${base_revision,,}",
];
pub(super) const TRUSTED_GATE_COMMAND_LINES: &[&str] = &["          /usr/bin/bash \"$trusted_bootstrap\" --root \"$candidate_root\" --maintainability"];
pub(super) const GPU_RELEASE_REVISION_ENVIRONMENT_LINES: &[&str] = &[
    "          test \"$(git rev-parse HEAD)\" = \"$GITHUB_SHA\"",
    "          printf 'CUDA_RELEASE_ROOT=%s\\n' \"$root\" >>\"$GITHUB_ENV\"",
    "          unset ORT_DYLIB_PATH LD_LIBRARY_PATH LD_PRELOAD",
    "          unset ORT_DYLIB_PATH LD_LIBRARY_PATH LD_PRELOAD",
    "          unset ORT_DYLIB_PATH LD_LIBRARY_PATH LD_PRELOAD",
    "          unset ORT_DYLIB_PATH LD_LIBRARY_PATH LD_PRELOAD",
    "            PATH=/usr/bin:/bin \\",
];

pub fn reject_checked_in_weakening(workspace: &Path) -> Result<BTreeSet<String>> {
    let surfaces = execution_surfaces(workspace)?;
    let audited_manifests = tracked_manifests(workspace)?.into_iter().collect::<BTreeSet<_>>();
    for path in surfaces.paths {
        if is_cargo_config(Path::new(&path)) {
            bail!("checked-in Cargo configuration {path:?} is unsupported because it can override lint policy");
        }
        if is_javascript(Path::new(&path)) {
            bail!("checked-in JavaScript command surface {path:?} is unsupported because process invocations cannot be audited as shell commands");
        }
        let source = fs::read_to_string(workspace.join(&path)).with_context(|| format!("read lint command execution surface {path}"))?;
        yaml::validate_execution_metadata(&path, &source)?;
        actions::validate_action_references(workspace, &surfaces.tracked_paths, &path, &source)?;
        make::validate_surface(Path::new(&path), &source)?;
        if has_sourced_file_indirection(&path, &source) {
            bail!("checked-in Rust command surface {path:?} uses unsupported sourced-file indirection");
        }
        let (selected_manifests, unresolved_manifest) = cargo_manifest_paths_for_surface(&path, &source);
        if unresolved_manifest || !selected_manifests.is_subset(&audited_manifests) {
            bail!("checked-in Rust command surface {path:?} selects a Cargo manifest outside the audited manifest inventory");
        }
        if weakening_token_for_surface(&path, &source) && !reviewed_dynamic_command_references_are_exact(&path, &source) {
            bail!("checked-in Rust command surface {path:?} contains a lint-weakening argument");
        }
        if weakening_environment_for_surface(&path, &source) && !scrubber_environment_references_are_exact(&path, &source) {
            bail!("checked-in Rust command surface {path:?} contains a lint-weakening environment channel");
        }
        let (sources, unresolved) = direct_rust_sources_for_surface(&path, &source);
        if unresolved {
            bail!("checked-in Rust command surface {path:?} contains a direct compiler invocation without auditable repository-relative .rs inputs");
        }
        if !sources.is_empty() {
            bail!("checked-in Rust command surface {path:?} directly compiles an opaque command helper; use an audited Cargo target instead");
        }
    }
    Ok(BTreeSet::new())
}

fn is_javascript(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "js" | "cjs" | "mjs"))
}

pub(super) fn weakening_environment(source: &str) -> bool {
    weakening_environment_names(source) || normalized_shell_tokens(source).iter().any(|token| weakening_environment_names(token))
}

pub(super) fn weakening_environment_for_surface(path: &str, source: &str) -> bool {
    if let Some(scripts) = package_script_commands(path, source) {
        return scripts.map_or(true, |scripts| scripts.iter().any(|script| weakening_environment_for_surface("", script)));
    }
    weakening_environment(source)
        || case_insensitive_environment_assignment(source)
        || yaml::environment_variables(path, source)
            .iter()
            .any(|(name, _)| is_weakening_environment_assignment_name(name))
        || quality_command_referenced(path, source) && path_environment_assignment(path, source)
}

fn case_insensitive_environment_assignment(source: &str) -> bool {
    environment_assignment_matches(source, is_weakening_environment_assignment_name)
}

fn path_environment_assignment(path: &str, source: &str) -> bool {
    let assignment_predicate: fn(&str) -> bool = if has_case_insensitive_environment_names(path) {
        is_path_environment_name
    } else {
        is_exact_path_environment_name
    };
    environment_assignment_matches(source, assignment_predicate) || yaml::environment_variables(path, source).iter().any(|(name, _)| is_path_environment_name(name))
}

fn has_case_insensitive_environment_names(path: &str) -> bool {
    let path = Path::new(path);
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "ps1" | "cmd" | "bat" | "yml" | "yaml"))
        || path.file_name().and_then(|name| name.to_str()) == Some("package.json")
}

fn environment_assignment_matches(source: &str, predicate: fn(&str) -> bool) -> bool {
    source.split(['\n', ';', '&', '|']).any(|segment| {
        let words = normalized_shell_words(segment);
        words
            .iter()
            .any(|word| word.split_once('=').map(|(name, _)| normalized_environment_name(name)).is_some_and(predicate))
            || words.windows(2).any(|tokens| tokens[1] == "=" && predicate(normalized_environment_name(&tokens[0])))
            || normalized_shell_tokens(segment)
                .windows(2)
                .any(|tokens| matches!(tokens[0].to_ascii_lowercase().as_str(), "export" | "unset" | "set" | "setenv") && predicate(normalized_environment_name(&tokens[1])))
    })
}

fn rust_tool_referenced(source: &str) -> bool {
    normalized_shell_tokens(source).iter().any(|token| {
        let token = normalized_environment_name(token).trim_matches(|character: char| matches!(character, '/' | '\\'));
        let basename = token.rsplit(['/', '\\']).next().unwrap_or(token).to_ascii_lowercase();
        matches!(
            basename.as_str(),
            "cargo"
                | "cargo.exe"
                | "cargo_executable"
                | "cargo-clippy"
                | "cargo-clippy.exe"
                | "cargo_clippy_executable"
                | "native_cargo"
                | "rustc"
                | "rustc.exe"
                | "rustc_executable"
                | "native_rustc"
                | "rustdoc"
                | "rustdoc.exe"
                | "rustdoc_executable"
                | "native_rustdoc"
                | "clippy-driver"
                | "clippy-driver.exe"
        )
    })
}

fn quality_command_referenced(path: &str, source: &str) -> bool {
    rust_tool_referenced(source) || arguments::contains_quality_command(source, arguments::has_case_insensitive_tool_names(path))
}

fn normalized_environment_name(name: &str) -> &str {
    let name = name.trim_matches(|character: char| matches!(character, '$' | '{' | '}' | '(' | ')' | ':' | '"' | '\''));
    match name.split_once(':') {
        Some((prefix, value)) if prefix.eq_ignore_ascii_case("env") => value,
        _ => name,
    }
}

fn weakening_environment_names(source: &str) -> bool {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|name| !name.is_empty())
        .filter(|name| name.bytes().any(|byte| byte.is_ascii_uppercase()))
        .any(is_weakening_environment_name)
}

const fn is_path_environment_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("PATH")
}

fn is_exact_path_environment_name(name: &str) -> bool {
    name == "PATH"
}

pub(super) fn scrubber_environment_references_are_exact(path: &str, source: &str) -> bool {
    let allowed = match path {
        "script/check-maintainability-bootstrap.sh" => BOOTSTRAP_ENVIRONMENT_LINES,
        "script/run-maintainability-gate.sh" => GATE_RUNNER_ENVIRONMENT_LINES,
        "script/run-source-safety.sh" => RUNNER_ENVIRONMENT_LINES,
        "script/tests/test_maintainability_bootstrap.sh" => BOOTSTRAP_TEST_ENVIRONMENT_LINES,
        "mise.toml" => MISE_ENVIRONMENT_LINES,
        ".github/workflows/ci.yml" => CI_TRUST_ENVIRONMENT_LINES,
        ".github/workflows/trusted-maintainability.yml" => TRUSTED_GATE_ENVIRONMENT_LINES,
        ".github/workflows/gpu-release-gate.yml" => GPU_RELEASE_REVISION_ENVIRONMENT_LINES,
        _ => return false,
    };
    let lines = source.lines().collect::<Vec<_>>();
    let quality_commands_are_referenced = quality_command_referenced(path, source);
    let yaml_environment_lines = yaml::environment_variables(path, source)
        .into_iter()
        .filter(|(name, _)| is_weakening_environment_assignment_name(name) || quality_commands_are_referenced && is_path_environment_name(name))
        .map(|(_, line)| line)
        .collect::<Vec<_>>();
    allowed.iter().all(|expected| {
        let expected_count = allowed.iter().filter(|candidate| *candidate == expected).count();
        lines.iter().filter(|line| *line == expected).count() == expected_count
    }) && lines
        .iter()
        .filter(|line| {
            weakening_environment_for_surface("", line) || quality_commands_are_referenced && path_environment_assignment("", line) || yaml_environment_lines.contains(line)
        })
        .all(|line| allowed.contains(line))
}

pub(super) fn reviewed_dynamic_command_references_are_exact(path: &str, source: &str) -> bool {
    let expected = match path {
        "script/run-maintainability-gate.sh" => GATE_RUNNER_COMMAND_LINES,
        "script/run-source-safety.sh" => RUNNER_COMMAND_LINES,
        ".github/workflows/trusted-maintainability.yml" => TRUSTED_GATE_COMMAND_LINES,
        _ => return false,
    };
    let lines = source.lines().collect::<Vec<_>>();
    if !expected.iter().all(|line| lines.iter().filter(|candidate| *candidate == line).count() == 1) {
        return false;
    }
    let remaining = lines
        .iter()
        .map(|line| {
            if expected.contains(line) {
                format!("{}:", &line[..line.len() - line.trim_start().len()])
            } else {
                (*line).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    !weakening_token_in_reviewed_shell_remainder(&remaining)
}

pub(super) fn is_execution_surface(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let lowercase_basename = basename.to_ascii_lowercase();
    if matches!(lowercase_basename.as_str(), "justfile" | ".justfile")
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("just"))
        || is_mise_config(path)
        || is_cargo_config(path)
        || path.starts_with(".github/workflows")
        || matches!(lowercase_basename.as_str(), "action.yml" | "action.yaml")
        || path.starts_with("script")
        || matches!(basename, "Makefile" | "makefile" | "GNUmakefile" | "package.json")
    {
        return true;
    }
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "cmd" | "bat" | "py" | "js" | "cjs" | "mjs" | "mk"
        )
    })
}

fn is_mise_config(path: &Path) -> bool {
    let lowercase = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let path = Path::new(&lowercase);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if matches!(basename, ".rtx.toml" | ".rtx.local.toml") || mise_project_basename(basename) {
        return true;
    }
    let parent = path.parent().and_then(Path::file_name).and_then(|name| name.to_str());
    if parent.is_some_and(|parent| matches!(parent, "mise" | ".mise")) && mise_directory_basename(basename) {
        return true;
    }
    has_toml_extension(basename) && path.parent().is_some_and(|parent| parent.ends_with(Path::new(".config/mise/conf.d")))
}

fn mise_project_basename(basename: &str) -> bool {
    let basename = basename.strip_prefix('.').unwrap_or(basename);
    basename == "mise.toml" || basename == "mise.local.toml" || basename.starts_with("mise.") && has_toml_extension(basename)
}

fn mise_directory_basename(basename: &str) -> bool {
    basename == "config.toml" || basename == "config.local.toml" || basename.starts_with("config.") && has_toml_extension(basename)
}

fn has_toml_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
}

fn is_cargo_config(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("config") || name.eq_ignore_ascii_case("config.toml"))
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".cargo"))
}
