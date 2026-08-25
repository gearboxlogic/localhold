#[derive(Clone, Copy)]
pub(super) struct Header<'a> {
    pub(super) name: &'a str,
    pub(super) prefixed: bool,
}

pub(super) fn parse_header(command: &str) -> Option<Header<'_>> {
    let command = command.trim();
    for (start, _) in command.char_indices().chain(std::iter::once((command.len(), '\0'))) {
        if start > 0 && !command[..start].chars().next_back().is_some_and(char::is_whitespace) {
            continue;
        }
        let candidate = &command[start..];
        let Some(name) = exact_function_header(candidate) else {
            continue;
        };
        let prefix = command[..start].trim();
        if prefix.is_empty() || control_prefix(prefix) {
            return Some(Header {
                name,
                prefixed: !prefix.is_empty(),
            });
        }
    }
    None
}

fn exact_function_header(command: &str) -> Option<&str> {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix("function").filter(|rest| rest.chars().next().is_some_and(char::is_whitespace)) {
        let (name, rest) = shell_name(rest.trim_start())?;
        let rest = rest.trim();
        return (rest.is_empty() || rest == "()").then_some(name);
    }
    let (name, rest) = shell_name(command)?;
    let rest = rest.trim_start().strip_prefix('(')?.trim_start().strip_prefix(')')?;
    rest.trim().is_empty().then_some(name)
}

fn shell_name(source: &str) -> Option<(&str, &str)> {
    let end = source
        .find(|character: char| !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')))
        .unwrap_or(source.len());
    let name = &source[..end];
    (!name.is_empty() && !name.as_bytes()[0].is_ascii_digit()).then(|| (name, &source[end..]))
}

fn control_prefix(prefix: &str) -> bool {
    prefix
        .split_whitespace()
        .all(|word| matches!(word.trim_matches([';', '{', '}']), "if" | "then" | "elif" | "else" | "while" | "until" | "do" | "!" | ""))
}

pub(super) fn looks_like_function_header(command: &str) -> bool {
    let command = command.trim();
    command.contains("function ") || command.strip_suffix(')').is_some_and(|header| header.trim_end().ends_with('('))
}

pub(super) fn previous_command(source: &str, before: usize) -> Option<&str> {
    let prefix = source[..before].trim_end();
    let start = prefix.rfind(['\n', ';', '&', '|']).map_or(0, |index| index + 1);
    let command = prefix[start..].trim();
    (!command.is_empty()).then_some(command)
}

pub(super) fn without_continuations(source: &str) -> String {
    source.replace("\\\r\n", "").replace("\\\n", "")
}

pub(super) fn preceded_by_nonsequential_operator(source: &str, command_start: usize) -> bool {
    let prefix = source[..command_start].trim_end_matches(char::is_whitespace);
    prefix.ends_with("&&") || prefix.ends_with("||") || prefix.ends_with('|') || prefix.ends_with('&')
}

pub(super) fn has_unsupported_body_suffix(source: &str, body_end: usize) -> bool {
    let rest = skip_horizontal_space_and_continuations(&source[body_end + 1..]);
    !rest.is_empty() && !rest.starts_with(['\n', '\r', ';', '#'])
}

fn skip_horizontal_space_and_continuations(mut source: &str) -> &str {
    loop {
        source = source.trim_start_matches([' ', '\t']);
        if let Some(rest) = strip_one_continuation(source) {
            source = rest;
        } else {
            return source;
        }
    }
}

pub(super) fn strip_one_continuation(source: &str) -> Option<&str> {
    source.strip_prefix("\\\r\n").or_else(|| source.strip_prefix("\\\n"))
}

pub(super) fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 1_u32;
    let mut parameter_depth = 0_u32;
    let mut lexical = MatchLexicalState::default();
    let mut index = open + 1;
    while index < source.len() {
        let character = source[index..].chars().next()?;
        if lexical.expansion_is_active()
            && let Some(span) = super::super::substitution::span_at(source, index)
        {
            let Ok(span) = span else {
                return None;
            };
            index = span.end + 1;
            continue;
        }
        if lexical.expansion_is_active() && super::expansion::at(source, index) {
            return None;
        }
        if lexical.advance(source, index, character, parameter_depth == 0) {
            index += character.len_utf8();
            continue;
        }
        let parameter = character == '{' && source.as_bytes().get(index.wrapping_sub(1)) == Some(&b'$');
        if parameter_depth > 0 {
            parameter_depth = match character {
                '{' if parameter => parameter_depth + 1,
                '}' => parameter_depth - 1,
                _ => parameter_depth,
            };
            index += character.len_utf8();
            continue;
        }
        if parameter {
            parameter_depth = 1;
        } else if character == '{' {
            depth += 1;
        } else if character == '}' && depth == 1 {
            return Some(index);
        } else if character == '}' {
            depth -= 1;
        }
        index += character.len_utf8();
    }
    None
}

#[derive(Default)]
struct MatchLexicalState {
    quote: Option<char>,
    escaped: bool,
    comment: bool,
    ansi_c_quote: Option<()>,
}

impl MatchLexicalState {
    fn expansion_is_active(&self) -> bool {
        !self.escaped && !self.comment && self.quote != Some('\'')
    }

    fn advance(&mut self, source: &str, index: usize, character: char, comments_enabled: bool) -> bool {
        if self.comment {
            self.comment = character != '\n';
            return true;
        }
        if self.escaped {
            self.escaped = false;
            return true;
        }
        if character == '\\' && (self.quote != Some('\'') || self.ansi_c_quote.is_some()) {
            self.escaped = true;
            return true;
        }
        if matches!(character, '\'' | '"') {
            if self.quote.is_none() {
                self.ansi_c_quote = (character == '\'' && ansi_c_quote_opener(source, index)).then_some(());
            } else if self.quote == Some(character) {
                self.ansi_c_quote = None;
            }
            self.quote = updated_quote(self.quote, character);
            return true;
        }
        if comments_enabled && self.quote.is_none() && character == '#' && starts_comment(source, index) {
            self.comment = true;
            return true;
        }
        self.quote.is_some()
    }
}

pub(super) fn ansi_c_quote_opener(source: &str, quote_index: usize) -> bool {
    let Some(dollar_index) = quote_index.checked_sub(1).filter(|index| source.as_bytes().get(*index) == Some(&b'$')) else {
        return false;
    };
    source[..dollar_index].bytes().rev().take_while(|byte| *byte == b'\\').count().is_multiple_of(2)
}

pub(super) fn starts_comment(source: &str, index: usize) -> bool {
    source[..index]
        .chars()
        .next_back()
        .is_none_or(|previous| previous.is_whitespace() || matches!(previous, ';' | '|' | '&' | '(' | ')' | '{' | '}'))
}

pub(super) fn updated_quote(quote: Option<char>, character: char) -> Option<char> {
    if quote == Some(character) { None } else { quote.or(Some(character)) }
}
