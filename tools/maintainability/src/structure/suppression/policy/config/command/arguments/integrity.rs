use super::{command_without_comment, is_environment_assignment, is_rust_tool_token, is_shell_command_prefix, tokens, tool_basename};

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
    let mut characters = line.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
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
        if quote.is_none() && character == '|' && characters.peek().is_some_and(|(_, next)| *next == '|') && is_quality_command(&line[..index], case_insensitive_tools) {
            return true;
        }
    }
    false
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
    tokens::source_command_tokens(command).iter().any(|tokens| {
        let Some(executable_index) = tokens.iter().position(|token| {
            let word = token.trim_matches(['(', ')', '{', '}']);
            !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word)
        }) else {
            return false;
        };
        if is_rust_tool_token(&tokens[executable_index], case_insensitive_tools) {
            return true;
        }
        tool_basename(&tokens[executable_index]).eq_ignore_ascii_case("just")
            && tokens[executable_index + 1..].iter().any(|token| {
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
    })
}

#[cfg(test)]
mod tests {
    use super::{declares_rust_tool_function, failure_masks_quality_command};

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
        assert!(failure_masks_quality_command("just check-quality || true", true));
        assert!(failure_masks_quality_command("just check-quality && echo completed || true", true));
        assert!(failure_masks_quality_command("cargo clippy --locked || true", true));
        assert!(!failure_masks_quality_command("echo cargo || true", true));
        assert!(!failure_masks_quality_command("printf '%s\n' 'just check || true'", true));
    }
}
