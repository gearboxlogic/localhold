use std::path::Path;

#[cfg(test)]
pub(in crate::structure::suppression::policy::config) fn weakening_token(source: &str) -> bool {
    weakening_token_with_case(source, false)
}

pub(in crate::structure::suppression::policy::config) fn weakening_token_for_surface(path: &str, source: &str) -> bool {
    let case_insensitive_tools = is_windows_command_surface(path);
    weakening_token_with_case(source, case_insensitive_tools)
        || super::yaml::folded_run_commands(path, source)
            .iter()
            .any(|command| weakening_token_with_case(command, case_insensitive_tools))
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
    command.contains("--cap-lints")
        || cargo_changes_directory_before_compiler_arguments(&tokens, case_insensitive_tools)
        || rust_response_file(&tokens, case_insensitive_tools)
        || tokens.iter().any(|token| {
            token == "-A"
                || token.starts_with("-A") && token.len() > 2
                || token == "--allow"
                || token == "-W"
                || token.starts_with("-W") && token.len() > 2
                || matches!(token.as_str(), "--warn" | "--force-warn")
        })
        || tokens.iter().any(|token| token.starts_with("--config")) && !is_cargo_deny_command(&tokens, case_insensitive_tools)
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

fn command_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            token.push(character);
            escaped = false;
        } else if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            } else {
                token.push(character);
            }
        } else if quote == Some('"') {
            if character == '"' {
                quote = None;
            } else if character == '\\' {
                escaped = true;
            } else {
                token.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '\\' {
            escaped = true;
        } else if character.is_whitespace() || character == '=' {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if escaped {
        token.push('\\');
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
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

pub(super) fn is_windows_command_surface(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "ps1" | "cmd" | "bat"))
}
