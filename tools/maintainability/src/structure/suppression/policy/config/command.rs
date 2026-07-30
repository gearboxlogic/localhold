use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

mod arguments;
mod surfaces;
mod yaml;
use arguments::has_case_insensitive_tool_names;
pub(super) use arguments::has_sourced_file_indirection;
#[cfg(test)]
pub(super) use arguments::weakening_token;
pub(super) use arguments::weakening_token_for_surface;
use arguments::{direct_rust_sources_for_surface, normalized_shell_tokens};
use surfaces::execution_surfaces;

pub(super) const BOOTSTRAP_ENVIRONMENT_LINES: &[&str] = &[
    "cargo_home=${CARGO_HOME:-}",
    "            RUSTFLAGS | RUSTDOCFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_ENCODED_RUSTDOCFLAGS | CARGO_BUILD_TARGET | CLIPPY_ARGS | CLIPPY_CONF_DIR | \\",
    "                RUSTC | RUSTDOC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTDOC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | \\",
    "                CARGO_BUILD_RUSTFLAGS | CARGO_BUILD_RUSTDOCFLAGS | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | CARGO_TARGET_*_RUSTDOCFLAGS | \\",
    "                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER)",
];
pub(super) const BOOTSTRAP_TEST_ENVIRONMENT_LINES: &[&str] = &[
    "CARGO_HOME='C:\\cargo-home' \\",
    "export CARGO_HOME=$cargo_home",
    "unset CARGO_HOME",
    "RUSTDOCFLAGS=untrusted CARGO_ENCODED_RUSTFLAGS=untrusted CARGO_ENCODED_RUSTDOCFLAGS=untrusted CLIPPY_CONF_DIR=untrusted \\",
    "    RUSTDOC=untrusted RUSTC_WRAPPER=untrusted CARGO_BUILD_RUSTDOC=untrusted CARGO_BUILD_RUSTDOCFLAGS=untrusted \\",
    "    CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_RUSTDOCFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \\",
    "    run_check -- bash -c 'for name in RUSTDOCFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS CLIPPY_CONF_DIR RUSTDOC RUSTC_WRAPPER CARGO_BUILD_RUSTDOC CARGO_BUILD_RUSTDOCFLAGS CARGO_TARGET_TEST_RUSTFLAGS CARGO_TARGET_TEST_RUSTDOCFLAGS CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do [[ ! -v $name ]] || exit 1; done' >/dev/null",
];
pub(super) const MISE_ENVIRONMENT_LINES: &[&str] = &["CARGO_HOME = \"{{ env.XDG_CACHE_HOME | default(value=env.HOME ~ \\\"/.cache\\\") }}/localhold/cargo\""];
pub(super) const CI_REVISION_ENVIRONMENT_LINES: &[&str] = &[
    "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
    "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}",
];
pub(super) const GPU_RELEASE_REVISION_ENVIRONMENT_LINES: &[&str] = &["          test \"$(git rev-parse HEAD)\" = \"$GITHUB_SHA\""];

pub fn reject_checked_in_weakening(workspace: &Path) -> Result<BTreeSet<String>> {
    let mut direct_rust_sources = BTreeSet::new();
    for path in execution_surfaces(workspace)? {
        if is_cargo_config(Path::new(&path)) {
            bail!("checked-in Cargo configuration {path:?} is unsupported because it can override lint policy");
        }
        let source = fs::read_to_string(workspace.join(&path)).with_context(|| format!("read lint command execution surface {path}"))?;
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
        direct_rust_sources.extend(sources);
    }
    Ok(direct_rust_sources)
}

pub(super) fn weakening_environment(source: &str) -> bool {
    weakening_environment_with_case(source, false)
}

pub(super) fn weakening_environment_for_surface(path: &str, source: &str) -> bool {
    weakening_environment_with_case(source, has_case_insensitive_tool_names(path))
}

fn weakening_environment_with_case(source: &str, case_insensitive: bool) -> bool {
    weakening_environment_names(source, case_insensitive) || normalized_shell_tokens(source).iter().any(|token| weakening_environment_names(token, case_insensitive))
}

fn weakening_environment_names(source: &str, case_insensitive: bool) -> bool {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|name| !name.is_empty())
        .filter(|name| case_insensitive || name.bytes().any(|byte| byte.is_ascii_uppercase()))
        .map(str::to_ascii_uppercase)
        .any(|name| {
            matches!(
                name.as_str(),
                "RUSTFLAGS"
                    | "CARGO_ENCODED_RUSTFLAGS"
                    | "RUSTDOCFLAGS"
                    | "CARGO_ENCODED_RUSTDOCFLAGS"
                    | "CLIPPY_ARGS"
                    | "CLIPPY_CONF_DIR"
                    | "RUSTC"
                    | "RUSTDOC"
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
        })
}

pub(super) fn scrubber_environment_references_are_exact(path: &str, source: &str) -> bool {
    let allowed = match path {
        "script/check-maintainability-bootstrap.sh" => BOOTSTRAP_ENVIRONMENT_LINES,
        "script/tests/test_maintainability_bootstrap.sh" => BOOTSTRAP_TEST_ENVIRONMENT_LINES,
        "mise.toml" => MISE_ENVIRONMENT_LINES,
        ".github/workflows/ci.yml" => CI_REVISION_ENVIRONMENT_LINES,
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
        || path.starts_with(".github/actions") && matches!(lowercase_basename.as_str(), "action.yml" | "action.yaml")
        || path.starts_with("script")
        || matches!(basename, "Makefile" | "makefile" | "GNUmakefile" | "package.json")
    {
        return true;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "sh" | "bash" | "zsh" | "fish" | "ps1" | "cmd" | "bat" | "py"))
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
