use std::collections::BTreeSet;

use super::{is_cargo_tool_token, is_environment_assignment, is_rust_tool_token, is_shell_command_prefix, tokens, tool_basename};

mod functions;

pub(super) fn declares_required_command_override(source: &str, case_insensitive_tools: bool) -> bool {
    functions::declares_required_command_override(source, case_insensitive_tools)
}

pub(super) fn has_command_hash_override(source: &str) -> bool {
    tokens::source_command_tokens(&tokens::without_noncommand_shell_data(source)).iter().any(|command| {
        executable_index(command).is_some_and(|index| {
            tool_basename(&command[index]).eq_ignore_ascii_case("hash")
                && command[index + 1..].iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
                    argument
                        .strip_prefix('-')
                        .is_some_and(|options| !options.is_empty() && options.bytes().all(|option| option.is_ascii_alphabetic()) && options.contains('p'))
                })
        })
    })
}

pub(super) fn failure_masks_quality_command(source: &str, case_insensitive_tools: bool, command_backticks: Option<bool>) -> bool {
    let source = tokens::without_noncommand_shell_data(source);
    if functions::has_unparsed_definition(&source) {
        return true;
    }
    let quality_functions = functions::quality_function_names(&source, case_insensitive_tools);
    if let Some(command_backticks) = command_backticks {
        let (substitutions, malformed) = tokens::command_substitution_commands(&source, command_backticks);
        if malformed
            || substitutions
                .iter()
                .any(|command| contains_quality_or_function_command(command, case_insensitive_tools, &quality_functions))
        {
            return true;
        }
    }
    if quality_command_runs_without_errexit(&source, case_insensitive_tools, &quality_functions) {
        return true;
    }
    if tokens::source_command_tokens(&source)
        .iter()
        .any(|command| control_flow_masks_quality_command(command, case_insensitive_tools, &quality_functions))
    {
        return true;
    }
    for line in source.lines() {
        if line_masks_quality_command(line, case_insensitive_tools, &quality_functions) {
            return true;
        }
    }
    false
}

fn quality_command_runs_without_errexit(source: &str, case_insensitive_tools: bool, quality_functions: &BTreeSet<String>) -> bool {
    let mut errexit = true;
    for command in tokens::source_command_tokens(source) {
        if !errexit && command_is_quality_or_function(&command, case_insensitive_tools, quality_functions) {
            return true;
        }
        update_errexit(&command, &mut errexit);
    }
    false
}

fn update_errexit(command: &[String], errexit: &mut bool) {
    let Some(index) = executable_index(command) else {
        return;
    };
    if !tool_basename(&command[index]).eq_ignore_ascii_case("set") {
        return;
    }
    let arguments = &command[index + 1..];
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            return;
        }
        if matches!(argument.as_str(), "-o" | "+o") {
            if arguments.get(index + 1).is_some_and(|option| option.eq_ignore_ascii_case("errexit")) {
                *errexit = argument.starts_with('-');
            }
            index += 2;
            continue;
        }
        if let Some(options) = argument.strip_prefix(['-', '+'])
            && !options.starts_with('o')
            && options.contains('e')
        {
            *errexit = argument.starts_with('-');
        }
        index += 1;
    }
}

fn line_masks_quality_command(line: &str, case_insensitive_tools: bool, quality_functions: &BTreeSet<String>) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = updated_quote(quote, character);
            continue;
        }
        if quote.is_none() && character == '#' {
            return false;
        }
        if quote.is_none() {
            let command = &line[..index];
            if operator_masks_failure(line, index, character) && contains_quality_or_function_command(command, case_insensitive_tools, quality_functions) {
                return true;
            }
            if operator_suppresses_function_errexit(line, index, character) && functions::source_calls(command, quality_functions) {
                return true;
            }
        }
    }
    false
}

fn operator_suppresses_function_errexit(line: &str, index: usize, character: char) -> bool {
    if character == '|' {
        return true;
    }
    if character != '&' {
        return false;
    }
    let previous = line[..index].chars().next_back();
    let next = line[index + character.len_utf8()..].chars().next();
    !matches!(previous, Some('<' | '>')) && next != Some('>')
}

fn operator_masks_failure(line: &str, index: usize, character: char) -> bool {
    if character == '|' {
        return true;
    }
    if character != '&' {
        return false;
    }
    let previous = line[..index].chars().next_back();
    let next = line[index + character.len_utf8()..].chars().next();
    !matches!(previous, Some('&' | '<' | '>')) && !matches!(next, Some('&' | '>'))
}

fn updated_quote(quote: Option<char>, character: char) -> Option<char> {
    if quote == Some(character) {
        None
    } else if quote.is_none() {
        Some(character)
    } else {
        quote
    }
}

fn contains_quality_or_function_command(source: &str, case_insensitive_tools: bool, quality_functions: &BTreeSet<String>) -> bool {
    tokens::source_command_tokens(source)
        .iter()
        .any(|command| command_is_quality_or_function(command, case_insensitive_tools, quality_functions))
}

pub(in crate::structure::suppression::policy::config::command) fn contains_quality_command(source: &str, case_insensitive_tools: bool) -> bool {
    tokens::source_command_tokens(&tokens::without_noncommand_shell_data(source))
        .iter()
        .any(|tokens| command_is_quality(tokens, case_insensitive_tools))
}

fn control_flow_masks_quality_command(command: &[String], case_insensitive_tools: bool, quality_functions: &BTreeSet<String>) -> bool {
    let Some(index) = quality_executable_index(command, case_insensitive_tools) else {
        return false;
    };
    command_is_quality_or_function(command, case_insensitive_tools, quality_functions)
        && command[..index].iter().any(|token| {
            matches!(
                token.trim_matches(['(', ')', '{', '}']).to_ascii_lowercase().as_str(),
                "!" | "if" | "elif" | "while" | "until"
            )
        })
}

fn command_is_quality_or_function(command: &[String], case_insensitive_tools: bool, quality_functions: &BTreeSet<String>) -> bool {
    command_is_quality(command, case_insensitive_tools) || functions::command_calls(command, quality_functions)
}

fn command_is_quality(command: &[String], case_insensitive_tools: bool) -> bool {
    let Some(executable_index) = quality_executable_index(command, case_insensitive_tools) else {
        return false;
    };
    let executable = &command[executable_index];
    let arguments = &command[executable_index + 1..];
    let executable_name = tool_basename(executable).to_ascii_lowercase();
    match super::references::wrapper::select(executable, &executable_name, arguments) {
        super::references::wrapper::Selection::NotWrapper => {}
        super::references::wrapper::Selection::NoCommand => return false,
        super::references::wrapper::Selection::Nested(nested) => return command_is_quality(nested, case_insensitive_tools),
        super::references::wrapper::Selection::Opaque => return command_is_quality(arguments, case_insensitive_tools),
    }
    if is_rust_tool_token(executable, case_insensitive_tools) {
        if !is_cargo_tool_token(executable, case_insensitive_tools) {
            return true;
        }
        return arguments
            .iter()
            .any(|token| matches!(token.to_ascii_lowercase().as_str(), "check" | "clippy" | "deny" | "fmt" | "test"));
    }
    tool_basename(executable).eq_ignore_ascii_case("just")
        && arguments.iter().any(|token| {
            matches!(
                token.as_str(),
                "check"
                    | "check-quality"
                    | "clippy"
                    | "deny"
                    | "dependency-unsafe"
                    | "fmt-check"
                    | "hygiene"
                    | "maintainability"
                    | "production-clippy"
                    | "test"
                    | "time-abstraction"
            )
        })
}

fn quality_executable_index(tokens: &[String], case_insensitive_tools: bool) -> Option<usize> {
    tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_quality_command_prefix(word, case_insensitive_tools) && !is_environment_assignment(word)
    })
}

fn is_quality_command_prefix(word: &str, case_insensitive_tools: bool) -> bool {
    is_shell_command_prefix(word)
        || case_insensitive_tools
            && matches!(
                word.to_ascii_lowercase().as_str(),
                "!" | "if" | "then" | "elif" | "while" | "until" | "do" | "command" | "exec" | "builtin" | "nohup"
            )
}

fn executable_index(tokens: &[String]) -> Option<usize> {
    tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word)
    })
}

#[cfg(test)]
mod tests {
    use super::{failure_masks_quality_command, has_command_hash_override};

    #[test]
    fn required_quality_command_failures_cannot_be_masked() {
        let masks = |source| failure_masks_quality_command(source, true, Some(true));
        assert!(masks("just check-quality | true"));
        assert!(masks("just check-quality || true"));
        assert!(masks("just check-quality &"));
        assert!(masks("! just check-quality"));
        assert!(masks("if just check-quality; then echo accepted; fi"));
        assert!(masks("while cargo clippy --locked; do echo retrying; done"));
        assert!(masks("until cargo test --locked; do echo retrying; done"));
        assert!(masks("env -u RUSTFLAGS just check-quality | true"));
        assert!(masks("timeout 5 just check-quality &"));
        assert!(masks("nice -n 5 cargo clippy --locked || true"));
        assert!(masks("time -p cargo test --locked | cat"));
        assert!(masks(r#"printf '%s\n' "$(just check-quality)""#));
        assert!(masks("printf '%s' `cargo clippy`"));
        assert!(masks(r#"printf '%s\n' "$(( $(cargo test) + 1 ))""#));
        assert!(masks("set +e; just check-quality; true"));
        assert!(masks("gate() {\n just check-quality\n true\n}\ngate || true"));
        assert!(masks("gate() { just check-quality; true; }\ntime gate && echo accepted"));
        assert!(masks("inner() { cargo test; }\nouter() { inner; true; }\nif outer; then :; fi"));
        assert!(masks("set +o errexit\ncargo clippy --locked"));
        assert!(masks("set +eux; cargo test --locked"));
        assert!(masks("IF (CARGO.EXE clippy --locked) { Write-Output accepted }"));
        assert!(masks("just check-quality && echo completed || true"));
        assert!(masks("cargo clippy --locked | cat"));
        assert!(masks("cargo clippy --locked || true"));
        assert!(!masks("cargo clippy --locked && echo completed"));
        assert!(!masks("cargo clippy --locked &>clippy.log"));
        assert!(!masks("cargo clippy --locked 2>&1"));
        assert!(!masks("cargo build | grep warning"));
        assert!(!masks("echo cargo || true"));
        assert!(!masks("if echo just check-quality; then echo informational; fi"));
        assert!(!masks("printf '%s\n' 'just check | true'"));
        assert!(!masks("printf '%s\n' 'just check || true'"));
        assert!(!masks("printf '%s\n' '$(just check-quality)'"));
        assert!(!masks(r#"printf '%s\n' "$(printf 'just check-quality')""#));
        assert!(!masks("set +e; set -e; just check-quality"));
        assert!(!masks("gate() {\n just check-quality\n}\ngate"));
        assert!(!masks("set +o errexit; set -o errexit; cargo clippy --locked"));
        assert!(!masks("set +x; cargo test --locked"));
        assert!(!masks("case \"$name\" in\n    CARGO | RUSTC | RUSTDOC) return ;;\nesac"));
    }

    #[test]
    fn shell_command_hash_overrides_fail_closed() {
        assert!(has_command_hash_override("hash -p /tmp/fake cargo"));
        assert!(has_command_hash_override("builtin hash -lp /tmp/fake cargo"));
        assert!(!has_command_hash_override("hash -r"));
        assert!(!has_command_hash_override("printf '%s\n' 'hash -p /tmp/fake cargo'"));
    }
}
