use std::collections::BTreeSet;

use super::{is_environment_assignment, is_rust_subcommand, is_rust_tool_token, is_shell_command_prefix, tokens};

pub(super) fn has_rust_tool_assignment_flow(source: &str, case_insensitive_tools: bool) -> bool {
    assigned_rust_tool_variables(source, case_insensitive_tools)
        .iter()
        .any(|name| references_variable(source, name))
}

fn assigned_rust_tool_variables(source: &str, case_insensitive_tools: bool) -> BTreeSet<String> {
    tokens::source_command_tokens(source)
        .into_iter()
        .flatten()
        .filter_map(|word| {
            let (name, value) = assignment(&word)?;
            let value = value.trim_matches(['\'', '"']);
            (is_rust_tool_token(value, case_insensitive_tools) || is_rust_subcommand(value)).then_some(name.to_owned())
        })
        .collect()
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let word = word.trim_matches(['(', ')', '{', '}', ';']);
    let (name, value) = word.split_once('=')?;
    let name = name.trim_start_matches('$');
    (!name.is_empty() && !value.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !name.as_bytes()[0].is_ascii_digit())
        .then_some((name, value))
}

fn references_variable(source: &str, name: &str) -> bool {
    tokens::source_command_tokens(source)
        .iter()
        .any(|command| dynamic_command_variable(command).is_some_and(|candidate| candidate.eq_ignore_ascii_case(name)))
}

fn dynamic_command_variable(command: &[String]) -> Option<&str> {
    if command.get(1).is_some_and(|token| token == "=") {
        return None;
    }
    for token in command {
        let word = token.trim_matches(['(', ')', '{', '}']);
        if word.is_empty() || is_shell_command_prefix(word) || is_environment_assignment(word) || assignment(word).is_some() {
            continue;
        }
        return variable_name(word);
    }
    None
}

fn variable_name(word: &str) -> Option<&str> {
    word.strip_prefix("${")
        .and_then(|word| word.strip_suffix('}'))
        .or_else(|| word.strip_prefix('$'))
        .or_else(|| word.strip_prefix('%').and_then(|word| word.strip_suffix('%')))
        .or_else(|| word.strip_prefix('!').and_then(|word| word.strip_suffix('!')))
}

#[cfg(test)]
mod tests {
    use super::has_rust_tool_assignment_flow;

    #[test]
    fn referenced_rust_tool_assignments_fail_closed() {
        assert!(has_rust_tool_assignment_flow("tool=cargo; sub=clippy; flag=-A; $tool $sub -- $flag warnings", false));
        assert!(has_rust_tool_assignment_flow("set TOOL=cargo\n%TOOL% build", true));
        assert!(!has_rust_tool_assignment_flow("tool=cargo\nprintf '%s\\n' configured", false));
        assert!(!has_rust_tool_assignment_flow("cargo_name=cargo\nprintf '%s\\n' \"$cargo_name\"", false));
        assert!(!has_rust_tool_assignment_flow("tool=node; $tool application.js", false));
    }
}
