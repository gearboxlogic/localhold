use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

mod actions;
mod arguments;
mod surfaces;
mod yaml;
pub(super) use arguments::has_sourced_file_indirection;
#[cfg(test)]
pub(super) use arguments::weakening_token;
pub(super) use arguments::weakening_token_for_surface;
use arguments::{direct_rust_sources_for_surface, normalized_shell_tokens};
use surfaces::execution_surfaces;

pub(super) const BOOTSTRAP_ENVIRONMENT_LINES: &[&str] = &[
    "cargo_home=${CARGO_HOME:-}",
    "    LOCALHOLD_MAINTAINABILITY_GIT=$git_executable",
    "    export LOCALHOLD_MAINTAINABILITY_GIT",
    "            BASH_ENV | RUSTFLAGS | RUSTDOCFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_ENCODED_RUSTDOCFLAGS | RUSTC_BOOTSTRAP | CARGO_BUILD_TARGET | CLIPPY_ARGS | CLIPPY_CONF_DIR | \\",
    "                RUSTC | RUSTDOC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTDOC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | \\",
    "                CARGO_BUILD_RUSTFLAGS | CARGO_BUILD_RUSTDOCFLAGS | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | CARGO_TARGET_*_RUSTDOCFLAGS | \\",
    "                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER | GIT_*)",
    "    if [[ -v GITHUB_ACTIONS || -v GITHUB_EVENT_PATH || -v GITHUB_SHA ]]; then",
    "        if [[ ${GITHUB_ACTIONS:-} != true || -z ${GITHUB_EVENT_PATH:-} || -z ${GITHUB_SHA:-} ]]; then",
    "        if [[ ! $GITHUB_SHA =~ ^[[:xdigit:]]{40}$ || ${checked_head,,} != \"${GITHUB_SHA,,}\" ]]; then",
    "            printf 'checked-out Git head revision differs from GITHUB_SHA before checker compilation\\n' >&2",
];
pub(super) const GATE_RUNNER_ENVIRONMENT_LINES: &[&str] = &[
    "readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}",
    "RUSTC=$native_rustc",
    "RUSTDOC=$native_rustdoc",
    "LOCALHOLD_MAINTAINABILITY_CARGO=$native_cargo",
    "export PATH CARGO RUSTC RUSTDOC RUSTFMT LOCALHOLD_MAINTAINABILITY_CARGO",
    "    for name in BASH_ENV RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS RUSTC_BOOTSTRAP CLIPPY_CONF_DIR GIT_DIR RUSTC_WRAPPER \\",
    "        RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTDOCFLAGS CARGO_TARGET_TEST_RUSTFLAGS \\",
    "        CARGO_TARGET_TEST_RUSTDOCFLAGS CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do",
    "    [[ -n $LOCALHOLD_MAINTAINABILITY_CARGO && -n $git_command ]]",
];
pub(super) const RUNNER_ENVIRONMENT_LINES: &[&str] = &[
    "readonly cargo_command=${LOCALHOLD_MAINTAINABILITY_CARGO:?maintainability bootstrap did not provide an absolute Cargo command}",
    "readonly git_command=${LOCALHOLD_MAINTAINABILITY_GIT:?maintainability bootstrap did not provide an absolute Git command}",
];
pub(super) const BOOTSTRAP_TEST_ENVIRONMENT_LINES: &[&str] = &[
    "unset GITHUB_ACTIONS GITHUB_EVENT_PATH GITHUB_SHA",
    "if GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=0000000000000000000000000000000000000000 run_check >/dev/null 2>&1; then",
    "    printf 'maintainability bootstrap accepted a checker revision other than GITHUB_SHA\\n' >&2",
    "GITHUB_ACTIONS=true GITHUB_EVENT_PATH=$event_path GITHUB_SHA=$test_head run_check >/dev/null",
    "export CARGO_HOME=$cargo_home",
    "unset CARGO_HOME",
    "BASH_ENV=$bash_env RUSTDOCFLAGS=untrusted CARGO_ENCODED_RUSTFLAGS=untrusted CARGO_ENCODED_RUSTDOCFLAGS=untrusted RUSTC_BOOTSTRAP=untrusted CLIPPY_CONF_DIR=untrusted GIT_DIR=untrusted \\",
    "    RUSTDOC=untrusted RUSTC_WRAPPER=untrusted CARGO_BUILD_RUSTDOC=untrusted CARGO_BUILD_RUSTDOCFLAGS=untrusted \\",
    "    CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_RUSTDOCFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \\",
    "    run_check --test-environment >/dev/null",
];
pub(super) const MISE_ENVIRONMENT_LINES: &[&str] = &["CARGO_HOME = \"{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \\\"/.cache\\\") }}/localhold/cargo\""];
pub(super) const CI_TRUST_ENVIRONMENT_LINES: &[&str] = &[
    "          BASH_ENV: ''",
    "          BASH_ENV: ''",
    "          BASH_ENV: ''",
    "          BASH_ENV: ''",
    "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
    "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
];
pub(super) const GPU_RELEASE_REVISION_ENVIRONMENT_LINES: &[&str] = &["          test \"$(git rev-parse HEAD)\" = \"$GITHUB_SHA\""];

pub fn reject_checked_in_weakening(workspace: &Path) -> Result<BTreeSet<String>> {
    for path in execution_surfaces(workspace)? {
        if is_cargo_config(Path::new(&path)) {
            bail!("checked-in Cargo configuration {path:?} is unsupported because it can override lint policy");
        }
        if is_javascript(Path::new(&path)) {
            bail!("checked-in JavaScript command surface {path:?} is unsupported because process invocations cannot be audited as shell commands");
        }
        let source = fs::read_to_string(workspace.join(&path)).with_context(|| format!("read lint command execution surface {path}"))?;
        yaml::validate_execution_metadata(&path, &source)?;
        if has_make_include_indirection(Path::new(&path), &source) {
            bail!("checked-in Make command surface {path:?} uses unsupported include indirection");
        }
        if has_sourced_file_indirection(&path, &source) {
            bail!("checked-in Rust command surface {path:?} uses unsupported sourced-file indirection");
        }
        if weakening_token_for_surface(&path, &source) {
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

fn has_make_include_indirection(path: &Path, source: &str) -> bool {
    if !is_make_surface(path) {
        return false;
    }
    source
        .lines()
        .filter(|line| !line.starts_with('\t'))
        .map(str::trim_start)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_whitespace().next())
        .any(|directive| matches!(directive, "include" | "-include" | "sinclude"))
}

fn is_make_surface(path: &Path) -> bool {
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(basename, "Makefile" | "makefile" | "GNUmakefile")
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mk"))
}

pub(super) fn weakening_environment(source: &str) -> bool {
    weakening_environment_names(source) || normalized_shell_tokens(source).iter().any(|token| weakening_environment_names(token))
}

pub(super) fn weakening_environment_for_surface(_path: &str, source: &str) -> bool {
    weakening_environment(source) || case_insensitive_environment_assignment(source)
}

fn case_insensitive_environment_assignment(source: &str) -> bool {
    source.split(['\n', ';', '&', '|']).any(|segment| {
        let assignment = segment
            .split_once('=')
            .and_then(|(left, _)| left.split_whitespace().next_back())
            .map(normalized_environment_name);
        assignment.is_some_and(is_weakening_environment_name)
            || normalized_shell_tokens(segment).windows(2).any(|tokens| {
                matches!(tokens[0].to_ascii_lowercase().as_str(), "export" | "unset" | "set" | "setenv") && is_weakening_environment_name(normalized_environment_name(&tokens[1]))
            })
    })
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

fn is_weakening_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "BASH_ENV"
            | "RUSTFLAGS"
            | "CARGO_ENCODED_RUSTFLAGS"
            | "RUSTDOCFLAGS"
            | "CARGO_ENCODED_RUSTDOCFLAGS"
            | "CLIPPY_ARGS"
            | "CLIPPY_CONF_DIR"
            | "RUSTC"
            | "RUSTDOC"
            | "RUSTC_BOOTSTRAP"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "CARGO_HOME"
            | "CARGO_BUILD_TARGET"
            | "CARGO_BUILD_RUSTFLAGS"
            | "CARGO_BUILD_RUSTDOCFLAGS"
            | "CARGO_BUILD_RUSTC"
            | "CARGO_BUILD_RUSTDOC"
            | "CARGO_BUILD_RUSTC_WRAPPER"
            | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
            | "GITHUB_ACTIONS"
            | "GITHUB_EVENT_PATH"
            | "GITHUB_SHA"
            | "LOCALHOLD_MAINTAINABILITY_BASE_REV"
            | "LOCALHOLD_MAINTAINABILITY_CARGO"
            | "LOCALHOLD_MAINTAINABILITY_GIT"
            | "MISE_CONFIG_DIR"
            | "MISE_CONFIG_FILE"
            | "MISE_CEILING_PATHS"
            | "MISE_DEFAULT_CONFIG_FILENAME"
            | "MISE_ENV_FILE"
            | "MISE_GLOBAL_CONFIG_FILE"
            | "MISE_GLOBAL_CONFIG_ROOT"
            | "MISE_OVERRIDE_CONFIG_FILENAMES"
            | "MISE_SYSTEM_CONFIG_DIR"
            | "MISE_SYSTEM_CONFIG_FILE"
            | "MISE_SYSTEM_DIR"
    ) || name.starts_with("CARGO_ALIAS_")
        || name.starts_with("CARGO_TARGET_") && (name.ends_with("_RUSTFLAGS") || name.ends_with("_RUSTDOCFLAGS") || name.ends_with("_LINKER") || name.ends_with("_RUNNER"))
        || name.starts_with("GIT_")
}

pub(super) fn scrubber_environment_references_are_exact(path: &str, source: &str) -> bool {
    let allowed = match path {
        "script/check-maintainability-bootstrap.sh" => BOOTSTRAP_ENVIRONMENT_LINES,
        "script/run-maintainability-gate.sh" => GATE_RUNNER_ENVIRONMENT_LINES,
        "script/run-source-safety.sh" => RUNNER_ENVIRONMENT_LINES,
        "script/tests/test_maintainability_bootstrap.sh" => BOOTSTRAP_TEST_ENVIRONMENT_LINES,
        "mise.toml" => MISE_ENVIRONMENT_LINES,
        ".github/workflows/ci.yml" => CI_TRUST_ENVIRONMENT_LINES,
        ".github/workflows/gpu-release-gate.yml" => GPU_RELEASE_REVISION_ENVIRONMENT_LINES,
        _ => return false,
    };
    let lines = source.lines().collect::<Vec<_>>();
    allowed.iter().all(|expected| {
        let expected_count = allowed.iter().filter(|candidate| *candidate == expected).count();
        lines.iter().filter(|line| *line == expected).count() == expected_count
    }) && lines.iter().filter(|line| weakening_environment(line)).all(|line| allowed.contains(line))
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
