use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

mod analysis;
mod dynamic;
mod dynamic_program;
mod integrity;
mod package_json;
mod powershell;
mod python;
mod references;
mod source;
mod tokens;
mod toolchain;
use analysis::Options as AnalysisOptions;
pub(super) use integrity::contains_quality_command;
pub(super) use references::{cargo_manifest_paths_for_surface, execution_inputs_for_surface};
use tokens::{command_tokens, command_without_comment};

#[cfg(test)]
pub(in crate::structure::suppression::policy::config) fn weakening_token(source: &str) -> bool {
    weakening_token_with_options(source, AnalysisOptions::strict())
}

pub(in crate::structure::suppression::policy::config) fn weakening_token_for_surface(path: &str, source: &str) -> bool {
    if dynamic_program::is_unanalyzed_path(path) {
        return true;
    }
    if is_python(path) && (python::has_opaque_process_arguments(path, source) || python::has_opaque_filesystem_write(path, source)) {
        return true;
    }
    if is_powershell(path) && powershell::has_constructed_rust_arguments(source) {
        return true;
    }
    if is_powershell(path) && powershell::has_unchecked_native_quality_command(source) {
        return true;
    }
    let mut options = AnalysisOptions::strict();
    options = options.with_case_insensitive_tools();
    if is_powershell(path) {
        options = options.allow_backticks();
    }
    if is_python(path) || is_windows_command(path) {
        options = options.ignore_command_substitutions();
    }
    if is_python(path) {
        options = options.ignore_function_definitions();
    }
    if is_just(path) && ignored_just_recipe_failure(source, options.case_insensitive_tools()) {
        return true;
    }
    if let Some(scripts) = package_json::script_commands(path, source) {
        let options = options.without_initial_errexit();
        return scripts.map_or(true, |scripts| {
            scripts.iter().any(|script| weakening_token_with_options(script, options.allow_just_interpolation()))
        });
    }
    if is_standalone_shell_surface(path) && !shebang_enables_errexit(source) {
        options = options.without_initial_errexit();
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        if super::yaml::powershell_run_commands(path, &source)
            .iter()
            .any(|command| powershell::has_unchecked_native_quality_command(command))
        {
            return true;
        }
        return embedded_commands
            .iter()
            .any(|command| weakening_token_with_options(command, options.allow_just_interpolation()) || powershell::has_constructed_rust_arguments(command));
    }
    weakening_token_with_options(&source, if is_just(path) { options } else { options.allow_just_interpolation() })
        || embedded_commands
            .iter()
            .any(|command| weakening_token_with_options(command, options.allow_just_interpolation()))
}

pub(super) fn weakening_token_in_reviewed_shell_remainder(source: &str) -> bool {
    let options = AnalysisOptions::strict().with_case_insensitive_tools();
    if dynamic::has_rust_tool_assignment_flow(source, true) {
        return true;
    }
    let logical = join_command_continuations(source);
    if integrity::declares_required_command_override(&logical, true)
        || integrity::has_command_hash_override(&logical)
        || integrity::failure_masks_quality_command(&logical, true, Some(true), true, true)
    {
        return true;
    }
    logical.lines().any(|line| weakening_token_with_options(line, options.nested()))
}

pub(super) fn direct_rust_sources_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = true;
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
        return scripts.map_or(true, |scripts| scripts.iter().any(|script| source::contains_command(script)));
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return embedded_commands.iter().any(|command| source::contains_command(command));
    }
    source::contains_command(&source) || embedded_commands.iter().any(|command| source::contains_command(command))
}

fn is_shell_command_prefix(word: &str) -> bool {
    matches!(word, "!" | "if" | "then" | "elif" | "while" | "until" | "do" | "command" | "exec" | "builtin" | "nohup") || word.starts_with('-')
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

fn is_standalone_shell_surface(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase();
    if matches!(basename.as_str(), "justfile" | ".justfile" | "makefile" | "gnumakefile" | "package.json") {
        return false;
    }
    path.extension().and_then(|extension| extension.to_str()).is_none_or(|extension| {
        !matches!(
            extension.to_ascii_lowercase().as_str(),
            "bat" | "cmd" | "json" | "just" | "make" | "mk" | "ps1" | "py" | "rs" | "toml" | "yml" | "yaml"
        )
    })
}

fn shebang_enables_errexit(source: &str) -> bool {
    let Some(interpreter) = source.lines().next().and_then(|line| line.strip_prefix("#!")) else {
        return false;
    };
    let words = interpreter.split_whitespace().collect::<Vec<_>>();
    let Some(shell_index) = words.iter().position(|word| matches!(tool_basename(word), "bash" | "dash" | "sh" | "zsh")) else {
        return false;
    };
    let arguments = &words[shell_index + 1..];
    arguments.iter().take_while(|word| **word != "--").any(|word| {
        word.strip_prefix('-')
            .is_some_and(|options| !options.starts_with('-') && !options.starts_with('o') && options.contains('e'))
    }) || arguments.windows(2).any(|pair| pair[0] == "-o" && pair[1].eq_ignore_ascii_case("errexit"))
}

fn normalized_source_for_surface(path: &str, source: &str) -> String {
    match Path::new(path).extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("ps1") => powershell::normalize_escapes(source),
        Some(extension) if matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat") => join_command_continuations(source),
        Some(extension) if extension.eq_ignore_ascii_case("py") => python::normalize_continuations(source),
        _ => source.to_owned(),
    }
}

fn weakening_token_with_options(source: &str, options: AnalysisOptions) -> bool {
    if dynamic::has_rust_tool_assignment_flow(source, options.case_insensitive_tools()) {
        return true;
    }
    let logical = join_command_continuations(source);
    if options.inspects_integrity()
        && (integrity::declares_required_command_override(&logical, options.case_insensitive_tools())
            || integrity::has_command_hash_override(&logical)
            || integrity::failure_masks_quality_command(
                &logical,
                options.case_insensitive_tools(),
                options.command_substitution_backticks(),
                options.inspects_function_definitions(),
                options.initial_errexit(),
            ))
    {
        return true;
    }
    if logical
        .split(['\n', ';', '&', '|'])
        .map(command_without_comment)
        .map(command_tokens)
        .any(|tokens| argument_feeder_invokes_rust_tool(&tokens, options.case_insensitive_tools()))
    {
        return true;
    }
    logical
        .split(['\n', ';', '&', '|'])
        .map(command_without_comment)
        .any(|command| weakening_rust_command(command, options))
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

fn weakening_rust_command(command: &str, options: AnalysisOptions) -> bool {
    if is_non_executing_assignment(command) {
        return false;
    }
    let tokens = command_tokens(command);
    if toolchain::uses_unreviewed_selector(&tokens, options.case_insensitive_tools()) || toolchain::uses_unreviewed_rustup_configuration(&tokens, options.case_insensitive_tools())
    {
        return true;
    }
    if tokens.iter().any(|token| nested_rust_command_is_weakening(command, token, options)) {
        return true;
    }
    let lint_option = tokens.iter().any(|token| is_lint_option(token));
    if !tokens
        .iter()
        .any(|token| is_rust_tool_token(token, options.case_insensitive_tools()) && !is_variable_assignment_target(command, token))
    {
        return has_dynamic_command_name(command, &tokens, options.rejects_backticks()) && (lint_option || tokens.iter().any(|token| is_rust_subcommand(token)));
    }
    forwards_dynamic_arguments(&tokens, options)
        || declares_rust_tool_alias(&tokens)
        || cargo_changes_directory_before_compiler_arguments(&tokens, options.case_insensitive_tools())
        || cargo_manifest_path_is_opaque(&tokens, options.case_insensitive_tools())
        || rust_response_file(&tokens, options.case_insensitive_tools())
        || has_pathname_expansion_arguments(&tokens, options.case_insensitive_tools())
        || has_brace_expansion_arguments(&tokens, options.case_insensitive_tools())
        || injects_crate_attribute(&tokens)
        || lint_option
        || tokens
            .iter()
            .enumerate()
            .any(|(index, token)| token.starts_with("--config") && !is_cargo_deny_config_argument(&tokens, index, options.case_insensitive_tools()))
}

fn nested_rust_command_is_weakening(command: &str, token: &str, options: AnalysisOptions) -> bool {
    if !token.chars().any(char::is_whitespace) || !nested_text_may_reference_rust_tool(token) {
        return false;
    }
    token == command || !options.can_descend() || weakening_token_with_options(token, options.nested())
}

fn nested_text_may_reference_rust_tool(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    ["cargo", "rustc", "rustdoc", "clippy-driver", "clippy"].iter().any(|tool| source.contains(tool))
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

fn forwards_dynamic_arguments(tokens: &[String], options: AnalysisOptions) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, options.case_insensitive_tools())) else {
        return false;
    };
    tokens[tool_index..].iter().enumerate().any(|(relative_index, token)| {
        let index = tool_index + relative_index;
        token.contains('$')
            || options.rejects_backticks() && token.contains('`')
            || options.rejects_just_interpolation()
                && (token.contains("{{") || token.contains("}}"))
                && template_interpolation_reaches_compiler(tokens, tool_index, index, options.case_insensitive_tools())
            || contains_cmd_delayed_expansion(token)
            || token.starts_with('@') && opaque_splat_reaches_compiler(tokens, tool_index, index, options.case_insensitive_tools())
            || token == "%*"
            || token.split_once('%').is_some_and(|(_, suffix)| !suffix.is_empty() && suffix.contains('%'))
    })
}

fn template_interpolation_reaches_compiler(tokens: &[String], tool_index: usize, interpolation_index: usize, case_insensitive_tools: bool) -> bool {
    if interpolation_index <= tool_index {
        return true;
    }
    if is_direct_compiler_token(&tokens[tool_index], case_insensitive_tools) {
        return true;
    }
    if !is_cargo_tool_token(&tokens[tool_index], case_insensitive_tools) {
        return false;
    }
    let preceding = &tokens[tool_index.saturating_add(1)..interpolation_index];
    preceding
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| matches!(argument.as_str(), "clippy" | "rustc" | "rustdoc"))
        || preceding.iter().all(|argument| argument.starts_with('-'))
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
    collect_direct_rust_sources_at_depth(source, case_insensitive_tools, sources, 0)
}

fn collect_direct_rust_sources_at_depth(source: &str, case_insensitive_tools: bool, sources: &mut BTreeSet<String>, depth: usize) -> bool {
    const MAX_NESTED_COMMAND_DEPTH: usize = 32;
    if depth == MAX_NESTED_COMMAND_DEPTH {
        return true;
    }
    let logical = join_command_continuations(source);
    let mut unresolved = false;
    for command in logical.split(['\n', ';', '&', '|']).map(command_without_comment) {
        let tokens = command_tokens(command);
        for token in &tokens {
            if token.chars().any(char::is_whitespace) && nested_text_may_reference_rust_tool(token) {
                unresolved |= token == source || collect_direct_rust_sources_at_depth(token, case_insensitive_tools, sources, depth + 1);
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

fn untrusted_directory_change_with_quality_dispatcher(source: &str, case_insensitive_tools: bool) -> bool {
    let commands = tokens::normalized_shell_commands(source);
    integrity::contains_quality_dispatcher(source, case_insensitive_tools)
        && commands
            .iter()
            .any(|tokens| is_directory_change_command(tokens) && !is_audited_repository_root_change(tokens, source))
}

fn is_directory_change_command(tokens: &[String]) -> bool {
    if env_changes_directory(tokens) {
        return true;
    }
    tokens
        .iter()
        .map(|word| word.trim_matches(['(', ')', '{', '}']))
        .find(|word| !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word))
        .is_some_and(|word| matches!(word.to_ascii_lowercase().as_str(), "cd" | "chdir" | "pushd" | "set-location"))
}

fn env_changes_directory(tokens: &[String]) -> bool {
    let Some(env_index) = tokens
        .iter()
        .position(|word| tool_basename(word.trim_matches(['(', ')', '{', '}'])).eq_ignore_ascii_case("env"))
    else {
        return false;
    };
    tokens[env_index.saturating_add(1)..].iter().take_while(|word| !is_rust_tool_token(word, true)).any(|word| {
        let word = word.to_ascii_lowercase();
        matches!(word.as_str(), "-c" | "--chdir") || word.starts_with("--chdir=") || word.starts_with("-c") && word.len() > 2
    })
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
    if !matches!(target, Some("$repository_root" | "$audit_root")) || words.next().is_some() {
        return false;
    }
    let lines = source.lines().map(str::trim).collect::<Vec<_>>();
    let assignment = r#"repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"#;
    let local_root_is_reviewed = lines.windows(2).filter(|lines| lines[0] == assignment && lines[1] == "readonly repository_root").count() == 1
        && lines.iter().filter(|line| line.starts_with("repository_root=")).count() == 1
        && lines.iter().filter(|line| **line == "readonly repository_root").count() == 1;
    local_root_is_reviewed || reviewed_external_audit_root(target, &lines)
}

fn reviewed_external_audit_root(target: Option<&str>, lines: &[&str]) -> bool {
    let expected: &[&str] = match target {
        Some("$repository_root") => &[
            "repository_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}",
            "if [[ $repository_root != /* && ! $repository_root =~ ^[[:alpha:]]:[/\\\\] ]] || [[ ! -d $repository_root || -L $repository_root ]]; then",
            "repository_root=$(cd -- \"$repository_root\" && pwd -P)",
            "LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT=$repository_root",
            "readonly implementation_root repository_root LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT",
            "export LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT",
        ],
        Some("$audit_root") => &[
            "audit_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}",
            "if [[ $audit_root != /* && ! $audit_root =~ ^[[:alpha:]]:[/\\\\] ]]; then",
            "audit_root=$(cd -- \"$audit_root\" && pwd -P)",
            "readonly implementation_root audit_root",
        ],
        _ => return false,
    };
    expected.iter().all(|expected| lines.iter().filter(|line| *line == expected).count() == 1)
}

fn is_python(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
}

fn is_windows_command(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat"))
}

fn is_just(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(basename.to_ascii_lowercase().as_str(), "justfile" | ".justfile")
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("just"))
}

fn ignored_just_recipe_failure(source: &str, case_insensitive_tools: bool) -> bool {
    source.lines().any(|line| {
        let command = line.trim_start();
        if command.len() == line.len() {
            return false;
        }
        let sigil_end = command.find(|character| !matches!(character, '@' | '-' | '?')).unwrap_or(command.len());
        let sigils = &command[..sigil_end];
        sigils.contains('-') && contains_quality_command(command[sigil_end..].trim_start(), case_insensitive_tools)
    })
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{collect_direct_rust_sources, untrusted_directory_change_with_quality_dispatcher, untrusted_directory_change_with_rust_tool, weakening_token_for_surface};

    #[test]
    fn protected_external_audit_roots_require_complete_validation() {
        let reviewed = r#"implementation_root=/trusted
repository_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}
if [[ $repository_root != /* && ! $repository_root =~ ^[[:alpha:]]:[/\\] ]] || [[ ! -d $repository_root || -L $repository_root ]]; then
    exit 1
fi
repository_root=$(cd -- "$repository_root" && pwd -P)
LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT=$repository_root
readonly implementation_root repository_root LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT
export LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT
cd -- "$repository_root"
cargo test --locked
"#;
        assert!(!untrusted_directory_change_with_rust_tool(reviewed, false));
        assert!(untrusted_directory_change_with_rust_tool(
            &reviewed.replace("[[ ! -d $repository_root || -L $repository_root ]]", "[[ ! -d $repository_root ]]"),
            false
        ));
    }

    #[test]
    fn quality_dispatchers_cannot_run_after_untrusted_directory_changes() {
        for source in [
            "cd quality/decoy; just check-quality",
            "pushd quality/decoy; make check-quality",
            "env --chdir=quality/decoy just maintainability",
        ] {
            assert!(untrusted_directory_change_with_quality_dispatcher(source, false), "{source}");
        }
        assert!(!untrusted_directory_change_with_quality_dispatcher("cd quality/decoy; just --version", false));
    }

    #[test]
    fn direct_source_discovery_bounds_nested_ansi_c_text() {
        let mut sources = BTreeSet::new();
        assert!(!collect_direct_rust_sources(r#"write_manifest $'[package]\nname = "checker"'"#, true, &mut sources));
        assert!(sources.is_empty());
        assert!(collect_direct_rust_sources(r"$'rustc source.rs'", true, &mut sources));
    }

    #[test]
    fn standalone_shells_cannot_assume_parent_errexit() {
        let masked = "#!/usr/bin/bash\ncargo clippy --locked -- -D warnings\ntrue\n";
        assert!(weakening_token_for_surface("quality/lint.data", masked));
        assert!(!weakening_token_for_surface("quality/lint.data", &masked.replace("#!/usr/bin/bash", "#!/usr/bin/bash -e")));
        assert!(!weakening_token_for_surface(
            "quality/lint.data",
            &masked.replace("#!/usr/bin/bash", "#!/usr/bin/bash\nset -e")
        ));
        assert!(!weakening_token_for_surface(
            "quality/lint.data",
            &masked.replace("#!/usr/bin/bash", "#!/usr/bin/env -S bash -euo pipefail")
        ));
        assert!(weakening_token_for_surface(
            "quality/lint.data",
            &masked.replace("#!/usr/bin/bash", "#!/usr/bin/pwsh -NoProfile")
        ));
    }
}
