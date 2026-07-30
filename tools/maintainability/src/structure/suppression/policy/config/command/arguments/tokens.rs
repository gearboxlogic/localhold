pub(super) fn normalized_shell_tokens(source: &str) -> Vec<String> {
    normalized_shell_commands(source).into_iter().flatten().collect()
}

pub(super) fn normalized_shell_commands(source: &str) -> Vec<Vec<String>> {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    logical
        .split(['\n', ';', '&', '|'])
        .map(|segment| command_tokens(command_without_comment(segment)))
        .collect()
}

pub(super) fn source_command_tokens(source: &str) -> Vec<Vec<String>> {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    logical
        .split(['\n', ';', '&', '|'])
        .map(|segment| shell_tokens(command_without_comment(segment), false))
        .collect()
}

pub(super) fn command_without_comment(segment: &str) -> &str {
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

pub(super) fn command_tokens(command: &str) -> Vec<String> {
    shell_tokens(command, true)
}

fn shell_tokens(command: &str, split_equals: bool) -> Vec<String> {
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
        } else if character.is_whitespace() || split_equals && character == '=' {
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
