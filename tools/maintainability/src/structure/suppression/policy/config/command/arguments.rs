use std::collections::BTreeSet;
use std::path::Path;

mod tokens;
use tokens::{command_tokens, command_without_comment};

#[cfg(test)]
pub(in crate::structure::suppression::policy::config) fn weakening_token(source: &str) -> bool {
    weakening_token_with_case(source, false)
}

pub(in crate::structure::suppression::policy::config) fn weakening_token_for_surface(path: &str, source: &str) -> bool {
    let case_insensitive_tools = has_case_insensitive_tool_names(path);
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return embedded_commands.iter().any(|command| weakening_token_with_case(command, case_insensitive_tools));
    }
    weakening_token_with_case(&source, case_insensitive_tools) || embedded_commands.iter().any(|command| weakening_token_with_case(command, case_insensitive_tools))
}

pub(super) fn direct_rust_sources_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = has_case_insensitive_tool_names(path);
    let source = normalized_source_for_surface(path, source);
    let mut sources = BTreeSet::new();
    let embedded_commands = super::yaml::run_commands(path, &source);
    let mut unresolved = !is_yaml(path) && collect_direct_rust_sources(&source, case_insensitive_tools, &mut sources);
    for command in embedded_commands {
        unresolved |= collect_direct_rust_sources(&command, case_insensitive_tools, &mut sources);
    }
    (sources, unresolved)
}

pub(super) fn normalized_shell_tokens(source: &str) -> Vec<String> {
    tokens::normalized_shell_tokens(source)
}

pub(in crate::structure::suppression::policy::config) fn has_sourced_file_indirection(path: &str, source: &str) -> bool {
    if !supports_shell_source(path) {
        return false;
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
    matches!(word, "!" | "if" | "then" | "elif" | "while" | "until" | "do" | "env") || word.starts_with('-')
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
    if Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ps1"))
    {
        source.replace("`\r\n", "").replace("`\n", "")
    } else {
        source.to_owned()
    }
}

fn weakening_token_with_case(source: &str, case_insensitive_tools: bool) -> bool {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    logical
        .split(['\n', ';', '&', '|'])
        .map(command_without_comment)
        .any(|command| weakening_rust_command(command, case_insensitive_tools))
}

fn weakening_rust_command(command: &str, case_insensitive_tools: bool) -> bool {
    let tokens = command_tokens(command);
    if tokens
        .iter()
        .any(|token| token.chars().any(char::is_whitespace) && weakening_token_with_case(token, case_insensitive_tools))
    {
        return true;
    }
    if !tokens.iter().any(|token| is_rust_tool_token(token, case_insensitive_tools)) {
        return false;
    }
    forwards_dynamic_arguments(&tokens)
        || declares_rust_tool_alias(&tokens)
        || cargo_changes_directory_before_compiler_arguments(&tokens, case_insensitive_tools)
        || rust_response_file(&tokens, case_insensitive_tools)
        || tokens
            .iter()
            .any(|token| token == "-A" || token.starts_with("-A") && token.len() > 2 || token == "-W" || token.starts_with("-W") && token.len() > 2 || is_long_lint_option(token))
        || tokens.iter().any(|token| token.starts_with("--config")) && !is_cargo_deny_command(&tokens, case_insensitive_tools)
}

fn forwards_dynamic_arguments(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        matches!(token.to_ascii_lowercase().as_str(), "$@" | "$*" | "${@}" | "${*}" | "$args" | "@args" | "$argv" | "%*") || token.contains("${args[") || token.contains("${argv[")
    })
}

fn declares_rust_tool_alias(tokens: &[String]) -> bool {
    tokens.first().is_some_and(|token| matches!(token.to_ascii_lowercase().as_str(), "alias" | "set-alias"))
}

fn is_long_lint_option(token: &str) -> bool {
    ["--allow", "--warn", "--force-warn", "--cap-lints"]
        .iter()
        .any(|option| token == *option || token.strip_prefix(option).is_some_and(|suffix| suffix.starts_with('=')))
}

fn collect_direct_rust_sources(source: &str, case_insensitive_tools: bool, sources: &mut BTreeSet<String>) -> bool {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
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

fn is_cargo_deny_command(tokens: &[String], case_insensitive_tools: bool) -> bool {
    tokens.windows(2).any(|pair| is_cargo_tool_token(&pair[0], case_insensitive_tools) && pair[1] == "deny")
}

pub(super) fn has_case_insensitive_tool_names(path: &str) -> bool {
    let path = Path::new(path);
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "ps1" | "cmd" | "bat"))
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yml" | "yaml"))
            && (path.starts_with(".github/workflows") || path.starts_with(".github/actions"))
}
