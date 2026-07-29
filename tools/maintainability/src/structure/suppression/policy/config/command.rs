use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::parse_nul_paths;

const BOOTSTRAP_ENVIRONMENT_LINES: &[&str] = &[
    "            RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_BUILD_TARGET | CLIPPY_ARGS | CLIPPY_CONF_DIR | RUSTC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | CARGO_BUILD_RUSTFLAGS | \\",
    "                CARGO_BUILD_RUSTC | CARGO_BUILD_RUSTC_WRAPPER | CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER | CARGO_ALIAS_* | CARGO_TARGET_*_RUSTFLAGS | \\",
    "                CARGO_TARGET_*_LINKER | CARGO_TARGET_*_RUNNER)",
];
const BOOTSTRAP_TEST_ENVIRONMENT_LINES: &[&str] = &[
    "CLIPPY_CONF_DIR=untrusted RUSTC_WRAPPER=untrusted CARGO_TARGET_TEST_RUSTFLAGS=untrusted CARGO_TARGET_TEST_LINKER=untrusted CARGO_TARGET_TEST_RUNNER=untrusted \\",
    "    run_check -- bash -c '[[ ! -v CLIPPY_CONF_DIR && ! -v RUSTC_WRAPPER && ! -v CARGO_TARGET_TEST_RUSTFLAGS && ! -v CARGO_TARGET_TEST_LINKER && ! -v CARGO_TARGET_TEST_RUNNER ]]' >/dev/null",
];

pub fn reject_checked_in_weakening(workspace: &Path) -> Result<()> {
    for path in execution_surfaces(workspace)? {
        let source = fs::read_to_string(workspace.join(&path)).with_context(|| format!("read lint command execution surface {path}"))?;
        let names_rust_tool = source.contains("cargo") || source.contains("rustc") || source.contains("clippy") || source.contains("CARGO") || source.contains("RUSTC");
        if names_rust_tool && weakening_token(&source) {
            bail!("checked-in Rust command surface {path:?} contains a lint-weakening argument");
        }
        if weakening_environment(&source) && !scrubber_environment_references_are_exact(&path, &source) {
            bail!("checked-in Rust command surface {path:?} contains a lint-weakening environment channel");
        }
    }
    Ok(())
}

fn execution_surfaces(workspace: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .context("list checked-in command execution surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing command execution surfaces");
    }
    parse_nul_paths(&output.stdout, is_execution_surface)
}

pub(super) fn weakening_token(source: &str) -> bool {
    source.contains("--cap-lints")
        || cargo_config_override(source)
        || source
            .split(|character: char| character.is_whitespace() || matches!(character, '\'' | '"' | '\\' | '='))
            .any(|token| {
                token == "-A"
                    || token.starts_with("-A") && token.len() > 2
                    || token == "--allow"
                    || token == "-W"
                    || token.starts_with("-W") && token.len() > 2
                    || matches!(token, "--warn" | "--force-warn")
            })
}

fn cargo_config_override(source: &str) -> bool {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    logical
        .split(['\n', ';', '&', '|'])
        .map(command_without_comment)
        .any(|command| command.contains("cargo") && command.contains("--config") && !is_cargo_deny_command(command))
}

fn command_without_comment(segment: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    let mut previous = None;
    for (index, character) in segment.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
        } else if character == '#' && quote.is_none() && previous.is_none_or(char::is_whitespace) {
            return &segment[..index];
        }
        previous = Some(character);
    }
    segment
}

fn is_cargo_deny_command(segment: &str) -> bool {
    let tokens = segment
        .split(|character: char| character.is_whitespace() || matches!(character, '\'' | '"' | '\\' | '='))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens.windows(2).any(|pair| pair == ["cargo", "deny"])
}

pub(super) fn weakening_environment(source: &str) -> bool {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|name| !name.is_empty())
        .filter(|name| name.bytes().any(|byte| byte.is_ascii_uppercase()))
        .map(str::to_ascii_uppercase)
        .any(|name| {
            matches!(
                name.as_str(),
                "RUSTFLAGS"
                    | "RUSTDOCFLAGS"
                    | "CLIPPY_ARGS"
                    | "CLIPPY_CONF_DIR"
                    | "RUSTC"
                    | "RUSTC_WRAPPER"
                    | "RUSTC_WORKSPACE_WRAPPER"
                    | "CARGO_BUILD_TARGET"
                    | "CARGO_BUILD_RUSTFLAGS"
                    | "CARGO_BUILD_RUSTC"
                    | "CARGO_BUILD_RUSTC_WRAPPER"
                    | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
            ) || name.starts_with("CARGO_ALIAS_")
                || name.starts_with("CARGO_TARGET_") && (name.ends_with("_RUSTFLAGS") || name.ends_with("_LINKER") || name.ends_with("_RUNNER"))
        })
}

pub(super) fn scrubber_environment_references_are_exact(path: &str, source: &str) -> bool {
    let allowed = match path {
        "script/check-maintainability-bootstrap.sh" => BOOTSTRAP_ENVIRONMENT_LINES,
        "script/tests/test_maintainability_bootstrap.sh" => BOOTSTRAP_TEST_ENVIRONMENT_LINES,
        _ => return false,
    };
    source.lines().filter(|line| weakening_environment(line)).all(|line| allowed.contains(&line))
}

pub(super) fn is_execution_surface(path: &str) -> bool {
    let path = Path::new(path);
    if path == Path::new("Justfile")
        || path == Path::new("mise.toml")
        || path.starts_with(".github/workflows")
        || path.starts_with("script")
        || matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("Makefile" | "makefile" | "GNUmakefile" | "package.json")
        )
    {
        return true;
    }
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("sh" | "bash" | "zsh" | "fish" | "ps1" | "cmd" | "bat" | "py")
    )
}
