use super::{command_without_comment, is_cargo_tool_token, is_environment_assignment, is_rust_tool_token, is_shell_command_prefix, tokens, tool_basename};

pub(super) fn declares_rust_tool_function(source: &str, case_insensitive_tools: bool) -> bool {
    let commands = source
        .lines()
        .flat_map(|line| command_without_comment(line).split(';'))
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>();
    commands.iter().enumerate().any(|(index, command)| {
        shell_function_signature(command).is_some_and(|(name, body_started)| {
            is_rust_tool_token(name, case_insensitive_tools) && (body_started || commands.get(index + 1).is_some_and(|next| next.starts_with('{')))
        })
    })
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

fn shell_function_signature(command: &str) -> Option<(&str, bool)> {
    let command = command.trim_start();
    if let Some(rest) = command.strip_prefix("function").filter(|rest| rest.chars().next().is_some_and(char::is_whitespace)) {
        let rest = rest.trim_start();
        let (name, rest) = shell_identifier(rest)?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("()").map_or(rest, str::trim_start);
        return (rest.is_empty() || rest.starts_with('{')).then_some((name, rest.starts_with('{')));
    }
    let (name, rest) = shell_identifier(command)?;
    let rest = rest.trim_start().strip_prefix('(')?.trim_start().strip_prefix(')')?.trim_start();
    (rest.is_empty() || rest.starts_with('{')).then_some((name, rest.starts_with('{')))
}

fn shell_identifier(source: &str) -> Option<(&str, &str)> {
    let end = source
        .find(|character: char| !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.'))
        .unwrap_or(source.len());
    (end > 0 && !source.as_bytes()[0].is_ascii_digit()).then(|| source.split_at(end))
}

pub(super) fn failure_masks_quality_command(source: &str, case_insensitive_tools: bool) -> bool {
    let source = tokens::without_noncommand_shell_data(source);
    if tokens::source_command_tokens(&source)
        .iter()
        .any(|command| control_flow_masks_quality_command(command, case_insensitive_tools))
    {
        return true;
    }
    for line in source.lines() {
        if line_masks_quality_command(line, case_insensitive_tools) {
            return true;
        }
    }
    false
}

fn line_masks_quality_command(line: &str, case_insensitive_tools: bool) -> bool {
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
        if quote.is_none() && operator_masks_failure(line, index, character) && is_quality_command(&line[..index], case_insensitive_tools) {
            return true;
        }
    }
    false
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

fn is_quality_command(command: &str, case_insensitive_tools: bool) -> bool {
    tokens::source_command_tokens(command)
        .iter()
        .any(|tokens| command_is_quality(tokens, case_insensitive_tools))
}

pub(in crate::structure::suppression::policy::config::command) fn contains_quality_command(source: &str, case_insensitive_tools: bool) -> bool {
    tokens::source_command_tokens(&tokens::without_noncommand_shell_data(source))
        .iter()
        .any(|tokens| command_is_quality(tokens, case_insensitive_tools))
}

fn control_flow_masks_quality_command(command: &[String], case_insensitive_tools: bool) -> bool {
    let Some(index) = quality_executable_index(command, case_insensitive_tools) else {
        return false;
    };
    command_is_quality(command, case_insensitive_tools)
        && command[..index].iter().any(|token| {
            matches!(
                token.trim_matches(['(', ')', '{', '}']).to_ascii_lowercase().as_str(),
                "!" | "if" | "elif" | "while" | "until"
            )
        })
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
    use super::{declares_rust_tool_function, failure_masks_quality_command, has_command_hash_override};

    #[test]
    fn rust_tool_functions_cannot_replace_required_executables() {
        assert!(declares_rust_tool_function("cargo() { :; }", true));
        let multiline = format!("cargo()\n{}\n    :\n{}", '{', '}');
        assert!(declares_rust_tool_function(&multiline, true));
        assert!(declares_rust_tool_function("function cargo { return }", true));
        assert!(!declares_rust_tool_function("printf '%s\n' 'cargo() {'", true));
        assert!(!declares_rust_tool_function("function report { return }", true));
    }

    #[test]
    fn required_quality_command_failures_cannot_be_masked() {
        assert!(failure_masks_quality_command("just check-quality | true", true));
        assert!(failure_masks_quality_command("just check-quality || true", true));
        assert!(failure_masks_quality_command("just check-quality &", true));
        assert!(failure_masks_quality_command("! just check-quality", true));
        assert!(failure_masks_quality_command("if just check-quality; then echo accepted; fi", true));
        assert!(failure_masks_quality_command("while cargo clippy --locked; do echo retrying; done", true));
        assert!(failure_masks_quality_command("until cargo test --locked; do echo retrying; done", true));
        assert!(failure_masks_quality_command("env -u RUSTFLAGS just check-quality | true", true));
        assert!(failure_masks_quality_command("timeout 5 just check-quality &", true));
        assert!(failure_masks_quality_command("nice -n 5 cargo clippy --locked || true", true));
        assert!(failure_masks_quality_command("time -p cargo test --locked | cat", true));
        assert!(failure_masks_quality_command("IF (CARGO.EXE clippy --locked) { Write-Output accepted }", true));
        assert!(failure_masks_quality_command("just check-quality && echo completed || true", true));
        assert!(failure_masks_quality_command("cargo clippy --locked | cat", true));
        assert!(failure_masks_quality_command("cargo clippy --locked || true", true));
        assert!(!failure_masks_quality_command("cargo clippy --locked && echo completed", true));
        assert!(!failure_masks_quality_command("cargo clippy --locked &>clippy.log", true));
        assert!(!failure_masks_quality_command("cargo clippy --locked 2>&1", true));
        assert!(!failure_masks_quality_command("cargo build | grep warning", true));
        assert!(!failure_masks_quality_command("echo cargo || true", true));
        assert!(!failure_masks_quality_command("if echo just check-quality; then echo informational; fi", true));
        assert!(!failure_masks_quality_command("printf '%s\n' 'just check | true'", true));
        assert!(!failure_masks_quality_command("printf '%s\n' 'just check || true'", true));
        assert!(!failure_masks_quality_command("case \"$name\" in\n    CARGO | RUSTC | RUSTDOC) return ;;\nesac", true));
    }

    #[test]
    fn shell_command_hash_overrides_fail_closed() {
        assert!(has_command_hash_override("hash -p /tmp/fake cargo"));
        assert!(has_command_hash_override("builtin hash -lp /tmp/fake cargo"));
        assert!(!has_command_hash_override("hash -r"));
        assert!(!has_command_hash_override("printf '%s\n' 'hash -p /tmp/fake cargo'"));
    }
}
