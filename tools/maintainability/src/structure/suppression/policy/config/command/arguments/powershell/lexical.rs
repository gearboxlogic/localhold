use std::iter::Peekable;
use std::str::Chars;

#[derive(Clone, Copy)]
enum Mode {
    Code,
    SingleQuoted,
    DoubleQuoted,
    SingleHereString,
    DoubleHereString,
    LineComment,
    BlockComment,
}

pub(super) fn has_code_semicolon(source: &str) -> bool {
    scan(source).0
}

pub(super) fn has_scriptblock_parameter_command(source: &str) -> bool {
    scan(source).1.iter().any(|word| matches!(word.as_str(), "foreach-object" | "where-object"))
}

fn scan(source: &str) -> (bool, Vec<String>) {
    let mut characters = source.chars().peekable();
    let mut mode = Mode::Code;
    let mut line_prefix_is_whitespace = true;
    let mut semicolon = false;
    let mut word = String::new();
    let mut words = Vec::new();
    while let Some(character) = characters.next() {
        if matches!(mode, Mode::Code | Mode::DoubleQuoted | Mode::DoubleHereString) && character == '`' {
            characters.next();
            continue;
        }
        if matches!(mode, Mode::Code) && (character.is_alphanumeric() || matches!(character, '-' | '_')) {
            word.push(character.to_ascii_lowercase());
            update_line_prefix(character, &mut line_prefix_is_whitespace);
            continue;
        }
        if matches!(mode, Mode::Code) && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if matches!(mode, Mode::Code) && character == ';' {
            semicolon = true;
        }
        match mode {
            Mode::Code => match character {
                '#' => mode = Mode::LineComment,
                '<' if characters.peek() == Some(&'#') => mode = Mode::BlockComment,
                '\'' => mode = Mode::SingleQuoted,
                '"' => mode = Mode::DoubleQuoted,
                '@' => mode = here_string_quote(&mut characters).map_or(Mode::Code, here_string_mode),
                _ => {}
            },
            Mode::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    characters.next();
                    continue;
                }
                mode = Mode::Code;
            }
            Mode::DoubleQuoted if character == '"' => mode = Mode::Code,
            Mode::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => mode = Mode::Code,
            Mode::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => mode = Mode::Code,
            Mode::LineComment if character == '\n' => mode = Mode::Code,
            Mode::BlockComment if character == '#' && characters.peek() == Some(&'>') => mode = Mode::Code,
            _ => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    if matches!(mode, Mode::Code) && !word.is_empty() {
        words.push(word);
    }
    (semicolon, words)
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

const fn here_string_mode(quote: char) -> Mode {
    if quote == '\'' { Mode::SingleHereString } else { Mode::DoubleHereString }
}

const fn update_line_prefix(character: char, line_prefix_is_whitespace: &mut bool) {
    if character == '\n' {
        *line_prefix_is_whitespace = true;
    } else if !character.is_whitespace() {
        *line_prefix_is_whitespace = false;
    }
}
