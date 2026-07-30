use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

mod dynamic;
mod package_json;
mod powershell;
mod python;
mod references;
mod tokens;
pub(super) use references::{cargo_manifest_paths_for_surface, literal_interpreter_scripts_for_surface};
use tokens::{command_tokens, command_without_comment};

#[cfg(test)]
pub(in crate::structure::suppression::policy::config) fn weakening_token(source: &str) -> bool {
    weakening_token_with_case(source, false, true)
}

pub(in crate::structure::suppression::policy::config) fn weakening_token_for_surface(path: &str, source: &str) -> bool {
    if is_python(path) && python::has_opaque_process_arguments(source) {
        return true;
    }
    let case_insensitive_tools = has_case_insensitive_tool_names(path);
    let reject_backticks = !is_powershell(path);
    if let Some(scripts) = package_json::script_commands(path, source) {
        return scripts.map_or(true, |scripts| {
            scripts.iter().any(|script| weakening_token_with_case(script, case_insensitive_tools, reject_backticks))
        });
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return embedded_commands
            .iter()
            .any(|command| weakening_token_with_case(command, case_insensitive_tools, reject_backticks));
    }
    weakening_token_with_case(&source, case_insensitive_tools, reject_backticks)
        || embedded_commands
            .iter()
            .any(|command| weakening_token_with_case(command, case_insensitive_tools, reject_backticks))
}

pub(super) fn direct_rust_sources_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = has_case_insensitive_tool_names(path);
    if let Some(scripts) = package_json::script_commands(path, source) {
        let Ok(scripts) = scripts else {
            return (BTreeSet::new(), true);
        };
        let mut sources = BTreeSet::new();
        let mut unresolved = false;
        for script in scripts {
            unresolved |= collect_direct_rust_sources(&script, case_insensitive_tools, &mut sources);
            unresolved |= untrusted_directory_change_with_rust_tool(&script, case_insensitive_tools);
        }
        return (sources, unresolved);
    }
    let source = normalized_source_for_surface(path, source);
    let mut sources = BTreeSet::new();
    let embedded_commands = super::yaml::run_commands(path, &source);
    let mut unresolved = !is_yaml(path) && collect_direct_rust_sources(&source, case_insensitive_tools, &mut sources);
    unresolved |= is_python(path) && !sources.is_empty();
    unresolved |= !is_yaml(path) && !is_python(path) && untrusted_directory_change_with_rust_tool(&source, case_insensitive_tools);
    for command in embedded_commands {
        unresolved |= collect_direct_rust_sources(&command, case_insensitive_tools, &mut sources);
        unresolved |= untrusted_directory_change_with_rust_tool(&command, case_insensitive_tools);
    }
    (sources, unresolved)
}

pub(super) fn normalized_shell_tokens(source: &str) -> Vec<String> {
    tokens::normalized_shell_tokens(source)
}

pub(super) fn normalized_shell_words(source: &str) -> Vec<String> {
    tokens::source_command_tokens(source).into_iter().flatten().collect()
}

pub(super) fn package_script_commands(path: &str, source: &str) -> Option<Result<Vec<String>>> {
    package_json::script_commands(path, source)
}

pub(in crate::structure::suppression::policy::config) fn has_sourced_file_indirection(path: &str, source: &str) -> bool {
    if !supports_shell_source(path) {
        return false;
    }
    if let Some(scripts) = package_json::script_commands(path, source) {
        return scripts.map_or(true, |scripts| scripts.iter().any(|script| contains_source_command(script)));
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return embedded_commands.iter().any(|command| contains_source_command(command));
    }
    contains_source_command(&source) || embedded_commands.iter().any(|command| contains_source_command(command))
}

fn contains_source_command(source: &str) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| {
        let mut words = command.iter().map(|word| word.trim_matches(['(', ')', '{', '}'])).filter(|word| !word.is_empty());
        loop {
            let Some(word) = words.next() else {
                return false;
            };
            if is_shell_command_prefix(word) || is_environment_assignment(word) {
                continue;
            }
            return matches!(word, "." | "source") && words.next().is_some();
        }
    })
}

fn is_shell_command_prefix(word: &str) -> bool {
    matches!(
        word,
        "!" | "if" | "then" | "elif" | "while" | "until" | "do" | "env" | "command" | "exec" | "builtin" | "nohup" | "sudo" | "time"
    ) || word.starts_with('-')
}

fn is_environment_assignment(word: &str) -> bool {
    word.split_once('=')
        .is_some_and(|(name, _)| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !name.as_bytes()[0].is_ascii_digit())
}

fn supports_shell_source(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if matches!(
        basename.to_ascii_lowercase().as_str(),
        "justfile" | ".justfile" | "makefile" | "gnumakefile" | "package.json"
    ) {
        return true;
    }
    path.extension().and_then(|extension| extension.to_str()).is_none_or(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "just" | "yml" | "yaml" | "toml"
        )
    })
}

fn is_yaml(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yml" | "yaml"))
}

fn normalized_source_for_surface(path: &str, source: &str) -> String {
    match Path::new(path).extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("ps1") => powershell::normalize_escapes(source),
        Some(extension) if matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat") => join_command_continuations(source),
        Some(extension) if extension.eq_ignore_ascii_case("py") => python::join_implicit_continuations(source),
        _ => source.to_owned(),
    }
}

fn weakening_token_with_case(source: &str, case_insensitive_tools: bool, reject_backticks: bool) -> bool {
    if dynamic::has_rust_tool_assignment_flow(source, case_insensitive_tools) {
        return true;
    }
    let logical = join_command_continuations(source);
    if logical
        .split(['\n', ';', '&', '|'])
        .map(command_without_comment)
        .map(command_tokens)
        .any(|tokens| argument_feeder_invokes_rust_tool(&tokens, case_insensitive_tools))
    {
        return true;
    }
    logical
        .split(['\n', ';', '&', '|'])
        .map(command_without_comment)
        .any(|command| weakening_rust_command(command, case_insensitive_tools, reject_backticks))
}

fn join_command_continuations(source: &str) -> String {
    source
        .replace("\\\r\n", "")
        .replace("\\\n", "")
        .replace("`\r\n", "")
        .replace("`\n", "")
        .replace("^\r\n", "")
        .replace("^\n", "")
}

fn weakening_rust_command(command: &str, case_insensitive_tools: bool, reject_backticks: bool) -> bool {
    if is_non_executing_assignment(command) {
        return false;
    }
    let tokens = command_tokens(command);
    if tokens
        .iter()
        .any(|token| token.chars().any(char::is_whitespace) && weakening_token_with_case(token, case_insensitive_tools, reject_backticks))
    {
        return true;
    }
    let lint_option = tokens.iter().any(|token| is_lint_option(token));
    if !tokens
        .iter()
        .any(|token| is_rust_tool_token(token, case_insensitive_tools) && !is_variable_assignment_target(command, token))
    {
        return has_dynamic_command_name(command, &tokens, reject_backticks) && (lint_option || tokens.iter().any(|token| is_rust_subcommand(token)));
    }
    forwards_dynamic_arguments(&tokens, case_insensitive_tools, reject_backticks)
        || declares_rust_tool_alias(&tokens)
        || cargo_changes_directory_before_compiler_arguments(&tokens, case_insensitive_tools)
        || cargo_manifest_path_is_opaque(&tokens, case_insensitive_tools)
        || rust_response_file(&tokens, case_insensitive_tools)
        || has_pathname_expansion_arguments(&tokens, case_insensitive_tools)
        || has_brace_expansion_arguments(&tokens, case_insensitive_tools)
        || injects_crate_attribute(&tokens)
        || lint_option
        || tokens
            .iter()
            .enumerate()
            .any(|(index, token)| token.starts_with("--config") && !is_cargo_deny_config_argument(&tokens, index, case_insensitive_tools))
}

fn is_non_executing_assignment(command: &str) -> bool {
    let Some((name, value)) = command.trim().split_once('=') else {
        return false;
    };
    !value.contains("$(")
        && !value.contains('`')
        && !value.chars().any(char::is_whitespace)
        && !name.is_empty()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

fn argument_feeder_invokes_rust_tool(tokens: &[String], case_insensitive_tools: bool) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        matches!(tool_basename(token).to_ascii_lowercase().as_str(), "xargs" | "xargs.exe" | "parallel" | "parallel.exe")
            && tokens[index.saturating_add(1)..]
                .iter()
                .any(|argument| is_rust_tool_token(argument, case_insensitive_tools))
    })
}

fn has_brace_expansion_arguments(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    tokens[tool_index.saturating_add(1)..].iter().any(|token| contains_shell_brace_expansion(token))
}

fn has_pathname_expansion_arguments(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    tokens[tool_index.saturating_add(1)..]
        .iter()
        .any(|token| token.contains(['*', '?', '[']) || contains_extended_glob(token))
}

fn contains_extended_glob(token: &str) -> bool {
    token.as_bytes().windows(2).any(|characters| matches!(characters, [b'?' | b'*' | b'+' | b'@' | b'!', b'(']))
}

fn contains_shell_brace_expansion(token: &str) -> bool {
    let mut remainder = token;
    while let Some((_, after_open)) = remainder.split_once('{') {
        let Some((body, after_close)) = after_open.split_once('}') else {
            return false;
        };
        if body.contains(',') || body.contains("..") {
            return true;
        }
        remainder = after_close;
    }
    false
}

fn has_dynamic_command_name(command: &str, tokens: &[String], reject_backticks: bool) -> bool {
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        let word = token.trim_matches(['(', ')', '{', '}']);
        if word.is_empty() || is_shell_command_prefix(word) || is_environment_assignment(word) {
            index += 1;
            continue;
        }
        if is_variable_assignment_target(command, word) {
            index += 1;
            continue;
        }
        return word.contains('$')
            || reject_backticks && word.contains('`')
            || contains_cmd_delayed_expansion(word)
            || word == "%*"
            || word.split_once('%').is_some_and(|(_, suffix)| !suffix.is_empty() && suffix.contains('%'));
    }
    false
}

fn is_variable_assignment_target(command: &str, word: &str) -> bool {
    let Some((left, _)) = command.split_once('=') else {
        return false;
    };
    let left = left.trim().trim_matches(['(', ')', '{', '}', '\'', '"']);
    left == word && (word.contains('$') || word.split_once('%').is_some_and(|(_, suffix)| suffix.contains('%')))
}

fn is_lint_option(token: &str) -> bool {
    token == "-A" || token.starts_with("-A") && token.len() > 2 || token == "-W" || token.starts_with("-W") && token.len() > 2 || is_long_lint_option(token)
}

fn forwards_dynamic_arguments(tokens: &[String], case_insensitive_tools: bool, reject_backticks: bool) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    tokens[tool_index..].iter().enumerate().any(|(relative_index, token)| {
        let index = tool_index + relative_index;
        token.contains('$')
            || reject_backticks && token.contains('`')
            || contains_cmd_delayed_expansion(token)
            || token.starts_with('@') && opaque_splat_reaches_compiler(tokens, tool_index, index, case_insensitive_tools)
            || token == "%*"
            || token.split_once('%').is_some_and(|(_, suffix)| !suffix.is_empty() && suffix.contains('%'))
    })
}

fn contains_cmd_delayed_expansion(token: &str) -> bool {
    token.bytes().filter(|byte| *byte == b'!').take(2).count() == 2
}

fn is_rust_subcommand(token: &str) -> bool {
    matches!(token.to_ascii_lowercase().as_str(), "clippy" | "rustc" | "rustdoc")
}

fn opaque_splat_reaches_compiler(tokens: &[String], tool_index: usize, splat_index: usize, case_insensitive_tools: bool) -> bool {
    if tokens[splat_index].eq_ignore_ascii_case("@args") || tokens[splat_index].eq_ignore_ascii_case("@argv") {
        return true;
    }
    let preceding_arguments = &tokens[tool_index.saturating_add(1)..splat_index];
    if !preceding_arguments.iter().any(|argument| argument == "--") || !is_cargo_tool_token(&tokens[tool_index], case_insensitive_tools) {
        return true;
    }
    preceding_arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| matches!(argument.as_str(), "clippy" | "rustc" | "rustdoc"))
}

fn cargo_manifest_path_is_opaque(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    let arguments = &tokens[tool_index.saturating_add(1)..];
    let Some(manifest_index) = arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .position(|argument| argument == "--manifest-path")
    else {
        return false;
    };
    arguments.get(manifest_index + 1).is_none_or(|manifest| !is_normalized_manifest_path(manifest))
}

fn is_normalized_manifest_path(manifest: &str) -> bool {
    let path = Path::new(manifest);
    !path.is_absolute()
        && !manifest.contains('\\')
        && path.components().all(|component| matches!(component, std::path::Component::Normal(_)))
        && path.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml")
}

fn declares_rust_tool_alias(tokens: &[String]) -> bool {
    tokens.first().is_some_and(|token| matches!(token.to_ascii_lowercase().as_str(), "alias" | "set-alias"))
}

fn is_long_lint_option(token: &str) -> bool {
    ["--allow", "--warn", "--force-warn", "--cap-lints"]
        .iter()
        .any(|option| token == *option || token.strip_prefix(option).is_some_and(|suffix| suffix.starts_with('=')))
}

fn injects_crate_attribute(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token == "-Zcrate-attr"
            || token.starts_with("-Zcrate-attr=")
            || token == "-Z" && tokens.get(index + 1).is_some_and(|value| value == "crate-attr" || value.starts_with("crate-attr="))
    })
}

fn collect_direct_rust_sources(source: &str, case_insensitive_tools: bool, sources: &mut BTreeSet<String>) -> bool {
    let logical = join_command_continuations(source);
    let mut unresolved = false;
    for command in logical.split(['\n', ';', '&', '|']).map(command_without_comment) {
        let tokens = command_tokens(command);
        for token in &tokens {
            if token.chars().any(char::is_whitespace) {
                unresolved |= collect_direct_rust_sources(token, case_insensitive_tools, sources);
            }
        }
        for compiler_index in tokens.iter().enumerate().filter_map(|(index, token)| {
            (is_literal_direct_compiler_token(token, case_insensitive_tools) && !tokens[..index].iter().any(|preceding| is_cargo_tool_token(preceding, case_insensitive_tools)))
                .then_some(index)
        }) {
            let arguments = &tokens[compiler_index.saturating_add(1)..];
            let candidates = arguments.iter().filter_map(|token| direct_source_candidate(token)).collect::<Vec<_>>();
            unresolved |= candidates.is_empty() && !is_informational_compiler_invocation(arguments);
            sources.extend(candidates);
        }
    }
    unresolved
}

fn untrusted_directory_change_with_rust_tool(source: &str, case_insensitive_tools: bool) -> bool {
    let commands = tokens::normalized_shell_commands(source);
    let has_rust_execution = commands.iter().any(|tokens| {
        tokens.iter().enumerate().any(|(index, token)| {
            is_cargo_tool_token(token, case_insensitive_tools)
                || is_literal_direct_compiler_token(token, case_insensitive_tools)
                    && !tokens[..index].iter().any(|preceding| is_cargo_tool_token(preceding, case_insensitive_tools))
                    && !is_informational_compiler_invocation(&tokens[index.saturating_add(1)..])
        })
    });
    has_rust_execution
        && commands
            .iter()
            .any(|tokens| is_directory_change_command(tokens) && !is_audited_repository_root_change(tokens, source))
}

fn is_directory_change_command(tokens: &[String]) -> bool {
    tokens
        .iter()
        .map(|word| word.trim_matches(['(', ')', '{', '}']))
        .find(|word| !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word))
        .is_some_and(|word| matches!(word.to_ascii_lowercase().as_str(), "cd" | "chdir" | "pushd" | "set-location"))
}

fn is_audited_repository_root_change(tokens: &[String], source: &str) -> bool {
    let mut words = tokens
        .iter()
        .map(|word| word.trim_matches(['(', ')', '{', '}']))
        .filter(|word| !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word));
    if !words.next().is_some_and(|word| word.eq_ignore_ascii_case("cd")) {
        return false;
    }
    let target = words.find(|word| *word != "--");
    if target != Some("$repository_root") || words.next().is_some() {
        return false;
    }
    let lines = source.lines().map(str::trim).collect::<Vec<_>>();
    let assignment = r#"repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"#;
    lines.windows(2).filter(|lines| lines[0] == assignment && lines[1] == "readonly repository_root").count() == 1
        && lines.iter().filter(|line| line.starts_with("repository_root=")).count() == 1
        && lines.iter().filter(|line| **line == "readonly repository_root").count() == 1
}

fn is_python(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
}

fn is_powershell(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ps1"))
}

fn is_literal_direct_compiler_token(token: &str, case_insensitive: bool) -> bool {
    !matches!(tool_basename(token), "RUSTC" | "RUSTDOC") && is_direct_compiler_token(token, case_insensitive)
}

fn direct_source_candidate(token: &str) -> Option<String> {
    let candidate = token.trim_matches(|character| matches!(character, '(' | ')' | ',' | ':'));
    Path::new(candidate)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
        .then(|| candidate.to_owned())
}

fn is_informational_compiler_invocation(arguments: &[String]) -> bool {
    arguments.is_empty()
        || arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "--version" | "-V" | "-vV" | "--help" | "-h" | "--print") || argument.starts_with("--print="))
}

fn cargo_changes_directory_before_compiler_arguments(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(cargo_index) = tokens.iter().position(|token| is_cargo_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    tokens[cargo_index.saturating_add(1)..]
        .iter()
        .take_while(|token| token.as_str() != "--")
        .any(|token| token == "-C" || token.starts_with("-C") && token.len() > 2 || token == "--change-directory" || token.starts_with("--change-directory="))
}

fn is_rust_tool_token(token: &str, case_insensitive: bool) -> bool {
    matches_tool_name(
        tool_basename(token),
        case_insensitive,
        &[
            "cargo",
            "cargo.exe",
            "cargo-clippy",
            "cargo-clippy.exe",
            "rustc",
            "rustc.exe",
            "rustdoc",
            "rustdoc.exe",
            "clippy-driver",
            "clippy-driver.exe",
            "CARGO",
            "RUSTC",
            "RUSTDOC",
        ],
    )
}

fn rust_response_file(tokens: &[String], case_insensitive_tools: bool) -> bool {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.starts_with('@') && token.len() > 1)
        .any(|(response_index, _)| {
            let preceding = &tokens[..response_index];
            preceding.iter().any(|token| is_direct_compiler_token(token, case_insensitive_tools))
                || preceding.iter().enumerate().any(|(index, token)| {
                    is_cargo_tool_token(token, case_insensitive_tools)
                        && preceding[index.saturating_add(1)..]
                            .iter()
                            .any(|argument| matches!(argument.as_str(), "rustc" | "rustdoc" | "clippy"))
                })
        })
}

fn is_direct_compiler_token(token: &str, case_insensitive: bool) -> bool {
    matches_tool_name(
        tool_basename(token),
        case_insensitive,
        &[
            "cargo-clippy",
            "cargo-clippy.exe",
            "rustc",
            "rustc.exe",
            "rustdoc",
            "rustdoc.exe",
            "clippy-driver",
            "clippy-driver.exe",
            "RUSTC",
            "RUSTDOC",
        ],
    )
}

fn is_cargo_tool_token(token: &str, case_insensitive: bool) -> bool {
    matches_tool_name(tool_basename(token), case_insensitive, &["cargo", "cargo.exe", "CARGO"])
}

fn matches_tool_name(actual: &str, case_insensitive: bool, expected: &[&str]) -> bool {
    expected
        .iter()
        .any(|candidate| actual == *candidate || case_insensitive && actual.eq_ignore_ascii_case(candidate))
}

fn tool_basename(token: &str) -> &str {
    let token = token.trim_matches(|character| matches!(character, '$' | '{' | '}' | '(' | ')' | ':' | ','));
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

fn is_cargo_deny_config_argument(tokens: &[String], config_index: usize, case_insensitive_tools: bool) -> bool {
    let Some(invocation_index) = tokens[..config_index].iter().rposition(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    is_cargo_tool_token(&tokens[invocation_index], case_insensitive_tools) && tokens.get(invocation_index + 1).is_some_and(|subcommand| subcommand == "deny")
}

pub(super) const fn has_case_insensitive_tool_names(_path: &str) -> bool {
    true
}
