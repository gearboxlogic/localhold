use std::iter::Peekable;
use std::str::Chars;

#[derive(Clone, Copy)]
enum State {
    Code,
    SingleQuoted,
    DoubleQuoted,
    SingleHereString,
    DoubleHereString,
    LineComment,
    BlockComment,
}

pub(super) fn normalize_escapes(source: &str) -> String {
    let mut normalized = String::with_capacity(source.len());
    let mut characters = source.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '`' {
            push_escaped(&mut normalized, &mut characters, &mut line_prefix_is_whitespace);
            continue;
        }

        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    normalized.push(character);
                    update_line_prefix(character, &mut line_prefix_is_whitespace);
                    normalized.push(characters.next().expect("peeked quote"));
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }

        normalized.push(character);
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    normalized
}

pub(super) fn has_constructed_rust_arguments(source: &str) -> bool {
    let source = normalize_escapes(source);
    has_opaque_dispatch(&source) || source.split(['\n', ';', '|']).any(|command| has_rust_tool(command) && has_argument_expression(command))
}

fn has_opaque_dispatch(source: &str) -> bool {
    let mut characters = source.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    let mut word = String::new();
    while let Some(character) = characters.next() {
        if matches!(state, State::Code) && (character.is_alphanumeric() || matches!(character, '-' | '_')) {
            word.push(character.to_ascii_lowercase());
            update_line_prefix(character, &mut line_prefix_is_whitespace);
            continue;
        }
        if matches!(state, State::Code) && process_api_word(&word) {
            return true;
        }
        word.clear();
        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                '&' if characters.peek() == Some(&'&') => {
                    characters.next();
                }
                '&' if dynamic_call_operator(characters.clone()) => return true,
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    characters.next();
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    matches!(state, State::Code) && process_api_word(&word)
}

fn process_api_word(word: &str) -> bool {
    matches!(word, "start-process" | "saps")
}

fn dynamic_call_operator(characters: Peekable<Chars<'_>>) -> bool {
    let command = call_operator_command(characters);
    let command = super::tokens::command_without_comment(&command);
    let tokens = super::tokens::command_tokens(command);
    let Some(command) = tokens.first() else {
        return false;
    };
    if dynamic_call_operand(command) {
        return true;
    }
    let Some(tool_index) = tokens.iter().position(|token| super::is_rust_tool_token(token, true)) else {
        return false;
    };
    tokens.iter().skip(tool_index + 1).any(|token| dynamic_call_operand(token))
}

fn call_operator_command(mut characters: Peekable<Chars<'_>>) -> String {
    let mut command = String::new();
    let mut quote = None;
    while let Some(character) = characters.next() {
        if quote == Some('\'') {
            if single_quoted_value_ends(character, &mut characters, &mut command) {
                quote = None;
            }
            continue;
        }
        if quote == Some('"') {
            command.push(character);
            if character == '"' {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            command.push(character);
        } else if character == '#' || matches!(character, '\n' | ';' | '|' | '&') {
            break;
        } else {
            command.push(character);
        }
    }
    command
}

fn single_quoted_value_ends(character: char, characters: &mut Peekable<Chars<'_>>, command: &mut String) -> bool {
    command.push(character);
    if character != '\'' {
        return false;
    }
    if characters.next_if_eq(&'\'').is_none() {
        return true;
    }
    command.push('\'');
    false
}

fn dynamic_call_operand(token: &str) -> bool {
    token.contains('$') || token.starts_with('@') || token.contains(['(', '{', '['])
}

fn has_rust_tool(command: &str) -> bool {
    super::tokens::command_tokens(command).iter().any(|token| super::is_rust_tool_token(token, true))
}

fn has_argument_expression(command: &str) -> bool {
    let mut characters = command.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '`' {
            characters.next();
            continue;
        }
        if matches!(state, State::Code) && matches!(character, '(' | '{') {
            return true;
        }
        if matches!(state, State::DoubleQuoted | State::DoubleHereString) && character == '$' && characters.peek() == Some(&'(') {
            return true;
        }
        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    characters.next();
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => state = State::Code,
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => state = State::Code,
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    false
}

fn here_string_quote(characters: &mut Peekable<Chars<'_>>) -> Option<char> {
    let quote = characters.peek().copied().filter(|character| matches!(character, '\'' | '"'))?;
    characters
        .clone()
        .skip(1)
        .take_while(|character| *character != '\n')
        .all(char::is_whitespace)
        .then_some(quote)
}

const fn here_string_state(quote: char) -> State {
    if quote == '\'' { State::SingleHereString } else { State::DoubleHereString }
}

fn push_escaped(normalized: &mut String, characters: &mut Peekable<Chars<'_>>, line_prefix_is_whitespace: &mut bool) {
    match characters.next() {
        Some('\r') if characters.peek() == Some(&'\n') => {
            characters.next();
        }
        Some('\n') | None => {}
        Some(escaped) => {
            normalized.push(escaped);
            update_line_prefix(escaped, line_prefix_is_whitespace);
        }
    }
}

const fn update_line_prefix(character: char, line_prefix_is_whitespace: &mut bool) {
    if character == '\n' {
        *line_prefix_is_whitespace = true;
    } else if !character.is_whitespace() {
        *line_prefix_is_whitespace = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{has_constructed_rust_arguments, normalize_escapes};

    #[test]
    fn escapes_are_normalized_only_where_powershell_expands_them() {
        assert_eq!(normalize_escapes("ca`rgo --a`llow"), "cargo --allow");
        assert_eq!(normalize_escapes("\"ca`rgo\""), "\"cargo\"");
        assert_eq!(normalize_escapes("'ca`rgo'"), "'ca`rgo'");
        assert_eq!(normalize_escapes("# ca`rgo\nca`rgo"), "# ca`rgo\ncargo");
        assert_eq!(normalize_escapes("<# ca`rgo #>\nca`rgo"), "<# ca`rgo #>\ncargo");
    }

    #[test]
    fn here_strings_keep_their_distinct_escape_semantics() {
        assert_eq!(normalize_escapes("@'\nca`rgo\n'@\nca`rgo"), "@'\nca`rgo\n'@\ncargo");
        assert_eq!(normalize_escapes("@\"\nca`rgo\n\"@\nca`rgo"), "@\"\ncargo\n\"@\ncargo");
    }

    #[test]
    fn constructed_rust_arguments_fail_closed() {
        assert!(has_constructed_rust_arguments("cargo clippy -- ('-' + 'A') warnings"));
        assert!(has_constructed_rust_arguments("cargo clippy -- \"$(Get-LintLevel)\" warnings"));
        assert!(has_constructed_rust_arguments(
            "Start-Process -FilePath ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('Y2FyZ28='))) -ArgumentList 'clippy','--','-A','warnings' -Wait"
        ));
        assert!(!has_constructed_rust_arguments("cargo clippy -- '-A' warnings"));
        assert!(!has_constructed_rust_arguments("Write-Output '(cargo clippy -- -A warnings)'"));
        assert!(!has_constructed_rust_arguments("Write-Output 'Start-Process cargo'"));
        assert!(!has_constructed_rust_arguments("# Start-Process cargo"));
        assert!(!has_constructed_rust_arguments("@'\nStart-Process cargo\n'@"));
        assert!(!has_constructed_rust_arguments("$actual = (Get-FileHash -Algorithm SHA256 $path).Hash"));
    }

    #[test]
    fn dynamic_call_operator_dispatch_fails_closed() {
        assert!(has_constructed_rust_arguments("& $tool $subcommand -- $flag warnings"));
        assert!(has_constructed_rust_arguments("& cargo $subcommand -- $flag warnings"));
        assert!(has_constructed_rust_arguments("& (Get-Command cargo) clippy -- -D warnings"));
        assert!(!has_constructed_rust_arguments("& cargo clippy -- -D warnings"));
        assert!(!has_constructed_rust_arguments("Write-Output '& $tool $flag'"));
        assert!(!has_constructed_rust_arguments("# & $tool $flag"));
        assert!(!has_constructed_rust_arguments("@'\n& $tool $flag\n'@"));
    }
}
