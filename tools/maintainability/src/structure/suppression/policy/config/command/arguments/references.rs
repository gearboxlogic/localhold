use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::{
    has_case_insensitive_tool_names, is_cargo_tool_token, is_environment_assignment, is_normalized_manifest_path, is_shell_command_prefix, is_yaml, matches_tool_name,
    normalized_source_for_surface, package_json, tokens, tool_basename,
};

pub(in crate::structure::suppression::policy::config::command) fn cargo_manifest_paths_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = has_case_insensitive_tool_names(path);
    if let Some(scripts) = package_json::script_commands(path, source) {
        let Ok(scripts) = scripts else {
            return (BTreeSet::new(), true);
        };
        return collect_cargo_manifest_paths(scripts.iter().map(String::as_str), case_insensitive_tools);
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return collect_cargo_manifest_paths(embedded_commands.iter().map(String::as_str), case_insensitive_tools);
    }
    collect_cargo_manifest_paths(std::iter::once(source.as_str()).chain(embedded_commands.iter().map(String::as_str)), case_insensitive_tools)
}

pub(in crate::structure::suppression::policy::config::command) fn literal_interpreter_scripts_for_surface(path: &str, source: &str) -> BTreeSet<String> {
    if let Some(scripts) = package_json::script_commands(path, source) {
        return scripts.map_or_else(|_| BTreeSet::new(), |scripts| collect_literal_interpreter_scripts(scripts.iter().map(String::as_str)));
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return collect_literal_interpreter_scripts(embedded_commands.iter().map(String::as_str));
    }
    collect_literal_interpreter_scripts(std::iter::once(source.as_str()).chain(embedded_commands.iter().map(String::as_str)))
}

fn collect_cargo_manifest_paths<'a>(sources: impl IntoIterator<Item = &'a str>, case_insensitive_tools: bool) -> (BTreeSet<String>, bool) {
    let mut paths = BTreeSet::new();
    let mut unresolved = false;
    for source in sources {
        collect_cargo_manifest_paths_from_source(source, case_insensitive_tools, &mut paths, &mut unresolved);
    }
    (paths, unresolved)
}

fn collect_cargo_manifest_paths_from_source(source: &str, case_insensitive_tools: bool, paths: &mut BTreeSet<String>, unresolved: &mut bool) {
    for tokens in tokens::normalized_shell_commands(source) {
        for nested in tokens.iter().filter(|token| token.chars().any(char::is_whitespace)) {
            collect_cargo_manifest_paths_from_source(nested, case_insensitive_tools, paths, unresolved);
        }
        let Some(tool_index) = tokens.iter().position(|token| is_manifest_capable_tool_token(token, case_insensitive_tools)) else {
            continue;
        };
        let arguments = &tokens[tool_index.saturating_add(1)..];
        let mut index = 0;
        while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
            let manifest = if argument == "--manifest-path" {
                index += 1;
                arguments.get(index).map(String::as_str)
            } else {
                argument.strip_prefix("--manifest-path=")
            };
            if let Some(manifest) = manifest {
                record_manifest_path(manifest, paths, unresolved);
            } else if argument == "--manifest-path" {
                *unresolved = true;
            }
            index += 1;
        }
    }
}

fn record_manifest_path(manifest: &str, paths: &mut BTreeSet<String>, unresolved: &mut bool) {
    if is_normalized_manifest_path(manifest) && !contains_dynamic_value(manifest) {
        paths.insert(manifest.to_owned());
    } else {
        *unresolved = true;
    }
}

fn is_manifest_capable_tool_token(token: &str, case_insensitive: bool) -> bool {
    is_cargo_tool_token(token, case_insensitive) || matches_tool_name(tool_basename(token), case_insensitive, &["cargo-clippy", "cargo-clippy.exe"])
}

fn collect_literal_interpreter_scripts<'a>(sources: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut scripts = BTreeSet::new();
    for source in sources {
        for tokens in tokens::source_command_tokens(source) {
            if let Some(script) = literal_interpreter_script(&tokens) {
                scripts.insert(script);
            }
        }
    }
    scripts
}

fn literal_interpreter_script(tokens: &[String]) -> Option<String> {
    let interpreter_index = tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word) && !tool_basename(word).eq_ignore_ascii_case("env")
    })?;
    let interpreter = tool_basename(&tokens[interpreter_index]).to_ascii_lowercase();
    let arguments = &tokens[interpreter_index.saturating_add(1)..];
    let candidate = match interpreter.as_str() {
        "bash" | "bash.exe" | "dash" | "dash.exe" | "fish" | "fish.exe" | "sh" | "sh.exe" | "zsh" | "zsh.exe" => shell_script(arguments)?,
        "python" | "python.exe" | "python3" | "python3.exe" => positional_script(arguments, &["-c", "-m"])?,
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => powershell_script(arguments)?,
        _ => return None,
    };
    normalized_literal_script(candidate)
}

fn shell_script(arguments: &[String]) -> Option<&str> {
    for (index, argument) in arguments.iter().enumerate() {
        let lowercase = argument.to_ascii_lowercase();
        if matches!(lowercase.as_str(), "-c" | "--command" | "-s" | "--stdin") || lowercase.starts_with('-') && !lowercase.starts_with("--") && lowercase[1..].contains(['c', 's'])
        {
            return None;
        }
        if argument == "--" {
            return arguments.get(index + 1).map(String::as_str);
        }
        if argument.starts_with(['-', '+']) {
            continue;
        }
        return Some(argument);
    }
    None
}

fn positional_script<'a>(arguments: &'a [String], non_file_modes: &[&str]) -> Option<&'a str> {
    let mut after_options = false;
    for argument in arguments {
        if !after_options && non_file_modes.iter().any(|mode| argument.eq_ignore_ascii_case(mode)) {
            return None;
        }
        if !after_options && argument == "--" {
            after_options = true;
            continue;
        }
        if !after_options && argument.starts_with('-') {
            continue;
        }
        return Some(argument);
    }
    None
}

fn powershell_script(arguments: &[String]) -> Option<&str> {
    for (index, argument) in arguments.iter().enumerate() {
        if matches!(argument.to_ascii_lowercase().as_str(), "-command" | "-c" | "-encodedcommand" | "-ec") {
            return None;
        }
        if matches!(argument.to_ascii_lowercase().as_str(), "-file" | "-f") {
            return arguments.get(index + 1).map(String::as_str);
        }
        if !argument.starts_with('-') {
            return Some(argument);
        }
    }
    None
}

fn normalized_literal_script(candidate: &str) -> Option<String> {
    if contains_dynamic_value(candidate) || candidate.contains('\\') {
        return None;
    }
    let path = Path::new(candidate);
    if path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            _ => return None,
        }
    }
    (!normalized.as_os_str().is_empty()).then(|| normalized.to_string_lossy().into_owned())
}

fn contains_dynamic_value(value: &str) -> bool {
    value.contains(['$', '`', '%', '!', '*', '?', '[', '{'])
}

#[cfg(test)]
mod tests {
    use super::{literal_interpreter_script, tokens};

    fn script(command: &str) -> Option<String> {
        literal_interpreter_script(&tokens::source_command_tokens(command)[0])
    }

    #[test]
    fn literal_interpreter_operands_distinguish_files_from_inline_code() {
        assert_eq!(script("bash quality/lint.txt"), Some("quality/lint.txt".to_owned()));
        assert_eq!(script("/usr/bin/env bash -e ./quality/lint.txt"), Some("quality/lint.txt".to_owned()));
        assert_eq!(script("bash -lc 'cargo clippy'"), None);
        assert_eq!(script("python -m quality.lint"), None);
        assert_eq!(script("pwsh -File quality/lint.ps1"), Some("quality/lint.ps1".to_owned()));
    }
}
