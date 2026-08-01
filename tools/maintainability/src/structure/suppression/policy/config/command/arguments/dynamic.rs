use std::collections::BTreeSet;

use super::{is_environment_assignment, is_rust_subcommand, is_rust_tool_token, is_shell_command_prefix, tokens, tool_basename};

mod builtins;

pub(super) fn has_rust_tool_assignment_flow(source: &str, case_insensitive_tools: bool) -> bool {
    let assigned_rust_tools = assigned_rust_tool_variables(source, case_insensitive_tools);
    if assigned_rust_tools.iter().any(|name| references_variable(source, name)) {
        return true;
    }
    let (computed, opaque_target) = builtins::assigned_variables(source);
    opaque_target || computed.iter().any(|name| references_variable(source, name)) || computed_variables_reach_rust_command(source, &computed, case_insensitive_tools)
}

pub(super) fn has_opaque_command_assignment_flow(path: &str, source: &str) -> bool {
    let assigned = assigned_opaque_command_variables(path, source);
    if assigned.iter().any(|name| references_variable(source, name)) {
        return true;
    }
    let (computed, opaque_target) = builtins::assigned_variables(source);
    opaque_target || computed.iter().any(|name| references_variable(source, name))
}

fn assigned_opaque_command_variables(path: &str, source: &str) -> BTreeSet<String> {
    tokens::source_command_tokens(source)
        .into_iter()
        .filter_map(|command| {
            command.iter().find_map(|word| {
                let (name, value) = assignment(word)?;
                opaque_command_assignment(path, name, value, &command).then_some(name.to_owned())
            })
        })
        .collect()
}

fn opaque_command_assignment(path: &str, name: &str, value: &str, command: &[String]) -> bool {
    let value = value.trim_matches(['\'', '"']);
    opaque_command_substitution(path, value, command) || !value.contains(['$', '`', '%', '!']) && !is_reviewed_literal_program_assignment(path, name, value)
}

fn is_reviewed_literal_program_assignment(path: &str, name: &str, value: &str) -> bool {
    matches!(
        (path, name, value),
        ("script/run-maintainability-gate.sh", "sha256_command", "/usr/bin/sha256sum")
            | ("script/run-maintainability-gate.sh", "uname_command", "/usr/bin/uname")
            | ("script/run-maintainability-gate.sh", "bash_command", "/usr/bin/bash")
            | ("script/run-maintainability-gate.sh", "chmod_command", "/usr/bin/chmod")
            | ("script/run-maintainability-gate.sh", "cp_command", "/usr/bin/cp")
            | ("script/run-maintainability-gate.sh", "ln_command", "/usr/bin/ln")
            | ("script/run-maintainability-gate.sh", "mkdir_command", "/usr/bin/mkdir")
            | ("script/run-maintainability-gate.sh", "mktemp_command", "/usr/bin/mktemp")
            | ("script/run-maintainability-gate.sh", "rm_command", "/usr/bin/rm")
            | ("script/run-maintainability-gate.sh", "curl_command", "/usr/bin/curl" | "/mingw64/bin/curl.exe")
            | (".github/workflows/ci.yml", "binary", "target/x86_64-pc-windows-msvc/release/hold.exe")
    )
}

fn opaque_command_substitution(path: &str, value: &str, command: &[String]) -> bool {
    let value = value.trim_matches(['\'', '"']);
    let resolver = value
        .split_once("$(")
        .map(|(_, value)| value)
        .or_else(|| value.split_once('`').map(|(_, value)| value))
        .map(|value| value.trim_matches(['\'', '"']));
    let Some(resolver) = resolver.filter(|resolver| !resolver.is_empty()) else {
        return false;
    };
    if resolver == "command" && command.iter().any(|word| word == "-v") {
        return false;
    }
    !is_reviewed_path_resolver(path, resolver)
}

fn is_reviewed_path_resolver(path: &str, resolver: &str) -> bool {
    matches!(
        path,
        "script/check-maintainability-bootstrap.sh" | "script/run-maintainability-gate.sh" | "script/run-source-safety.sh" | "script/tests/test_maintainability_bootstrap.sh"
    ) && matches!(resolver, "trusted_system_command" | "authenticated_tool" | "sha256_file")
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

fn computed_variables_reach_rust_command(source: &str, assigned: &BTreeSet<String>, case_insensitive_tools: bool) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| {
        let invokes_rust = command.iter().any(|word| is_rust_tool_token(tool_basename(word), case_insensitive_tools));
        invokes_rust
            && assigned
                .iter()
                .any(|name| command.iter().any(|word| references_named_variable(word, name, case_insensitive_tools)))
    })
}

fn references_named_variable(word: &str, name: &str, case_insensitive: bool) -> bool {
    let bytes = word.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        if !matches!(byte, b'$' | b'%' | b'!') {
            return false;
        }
        let mut start = index + 1;
        if *byte == b'$' && bytes.get(start) == Some(&b'{') {
            start += 1;
        }
        let mut end = start;
        while bytes.get(end).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
            end += 1;
        }
        let candidate = &word[start..end];
        !candidate.is_empty() && if case_insensitive { candidate.eq_ignore_ascii_case(name) } else { candidate == name }
    })
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let word = word.trim_matches(['(', ')', '{', '}', ';']);
    let (name, value) = word.split_once('=')?;
    let name = name.trim_start_matches('$');
    (!name.is_empty() && !value.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !name.as_bytes()[0].is_ascii_digit())
        .then_some((name, value))
}

fn references_variable(source: &str, name: &str) -> bool {
    tokens::source_command_tokens(&without_extended_test_bodies(source))
        .iter()
        .any(|command| dynamic_command_variable(command).is_some_and(|candidate| candidate.eq_ignore_ascii_case(name)))
}

fn without_extended_test_bodies(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut characters = source.chars().peekable();
    let mut quote = None;
    let mut escaped = false;
    let mut in_test = false;
    while let Some(character) = characters.next() {
        if escaped {
            output.push(if in_test && character != '\n' { ' ' } else { character });
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            output.push(if in_test { ' ' } else { character });
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
            output.push(if in_test { ' ' } else { character });
            continue;
        }
        if quote.is_none() && character == '[' && characters.peek() == Some(&'[') {
            output.extend([' ', ' ']);
            characters.next();
            in_test = true;
            continue;
        }
        if quote.is_none() && in_test && character == ']' && characters.peek() == Some(&']') {
            output.extend([' ', ' ']);
            characters.next();
            in_test = false;
            continue;
        }
        output.push(if in_test && character != '\n' { ' ' } else { character });
    }
    output
}

fn dynamic_command_variable(command: &[String]) -> Option<&str> {
    if command.get(1).is_some_and(|token| token == "=") {
        return None;
    }
    let mut substitution_depth = 0_usize;
    let mut in_backticks = false;
    for token in command {
        let word = token.trim_matches(['(', ')']);
        if substitution_depth > 0 || in_backticks {
            update_substitution_state(token, &mut substitution_depth, &mut in_backticks);
            continue;
        }
        if word.is_empty() || is_shell_command_prefix(word) || is_environment_assignment(word) || assignment(word).is_some() {
            update_substitution_state(token, &mut substitution_depth, &mut in_backticks);
            continue;
        }
        return variable_name(word);
    }
    None
}

fn update_substitution_state(word: &str, depth: &mut usize, in_backticks: &mut bool) {
    for character in word.chars() {
        if character == '`' {
            *in_backticks = !*in_backticks;
        }
    }
    *depth = depth.saturating_add(word.matches("$(").count()).saturating_sub(word.matches(')').count());
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
    use super::{has_opaque_command_assignment_flow, has_rust_tool_assignment_flow};

    #[test]
    fn referenced_rust_tool_assignments_fail_closed() {
        assert!(has_rust_tool_assignment_flow("tool=cargo; sub=clippy; flag=-A; $tool $sub -- $flag warnings", false));
        assert!(has_rust_tool_assignment_flow("set TOOL=cargo\n%TOOL% build", true));
        assert!(has_rust_tool_assignment_flow(
            "printf -v tool %b cargo; printf -v sub %b clippy; printf -v option %b A; \"$tool\" \"$sub\" -- \"-$option\" warnings",
            false
        ));
        assert!(has_rust_tool_assignment_flow("printf -v option %b A; cargo clippy -- \"-$option\" warnings", false));
        assert!(has_rust_tool_assignment_flow(
            "printf -v option %b A; cargo clippy -- \"$unrelated-$option\" warnings",
            false
        ));
        assert!(has_rust_tool_assignment_flow("IFS= read -r tool <<<cargo; \"$tool\" check", false));
        assert!(!has_rust_tool_assignment_flow("tool=cargo\nprintf '%s\\n' configured", false));
        assert!(!has_rust_tool_assignment_flow("cargo_name=cargo\nprintf '%s\\n' \"$cargo_name\"", false));
        assert!(!has_rust_tool_assignment_flow("tool=node; $tool application.js", false));
    }

    #[test]
    fn opaque_command_assignment_flows_fail_closed() {
        assert!(has_opaque_command_assignment_flow("script/check.sh", "runner=$(cat quality/lint.txt); $runner"));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "runner=`sed -n 1p quality/lint.txt`; ${runner}"));
        assert!(has_opaque_command_assignment_flow(
            "script/check-maintainability-bootstrap.sh",
            "resolver=/tmp/unreviewed; runner=$($resolver); $runner"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/check.sh",
            "runner=$(cat quality/lint.txt); if [[ -n $PATH ]]; then $runner; fi"
        ));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "runner=sh; \"$runner\" quality/lint.txt"));
        assert!(has_opaque_command_assignment_flow("script/check.sh", "tool=node; $tool application.js"));
        assert!(!has_opaque_command_assignment_flow(
            "script/check.sh",
            "kernel=$(/usr/bin/uname -s); if [[ $kernel == Linux || $kernel == MINGW* ]]; then true; fi"
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/claude-review.sh",
            "claude_binary=$(command -v claude || true); \"$claude_binary\""
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/check-maintainability-bootstrap.sh",
            "bash_command=$(trusted_system_command bash); \"$bash_command\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/run-maintainability-gate.sh",
            "readonly bash_command=/usr/bin/bash; \"$bash_command\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            ".github/workflows/ci.yml",
            "binary=target/x86_64-pc-windows-msvc/release/hold.exe; \"$binary\" --version"
        ));
        assert!(has_opaque_command_assignment_flow(
            "script/run-maintainability-gate.sh",
            "runner=/usr/bin/bash; \"$runner\" quality/lint.txt"
        ));
        assert!(has_opaque_command_assignment_flow(
            ".github/workflows/ci.yml",
            "binary=target/x86_64-pc-windows-msvc/release/unreviewed.exe; \"$binary\" --version"
        ));
        assert!(!has_opaque_command_assignment_flow(
            "script/tests/test_maintainability_bootstrap.sh",
            "repository_root=/reviewed; check=\"$repository_root/script/check.sh\"; \"$check\""
        ));
    }
}
