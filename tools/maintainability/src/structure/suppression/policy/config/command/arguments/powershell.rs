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
    use super::normalize_escapes;

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
}
