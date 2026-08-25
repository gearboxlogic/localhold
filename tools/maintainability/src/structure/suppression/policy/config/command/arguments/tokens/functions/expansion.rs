pub(super) fn has_current_shell_substitution(source: &str) -> bool {
    let mut lexical = LexicalState::default();
    let mut index = 0;
    while index < source.len() {
        let Some(character) = source[index..].chars().next() else {
            return false;
        };
        if lexical.can_open_substitution()
            && let Some(span) = super::super::substitution::span_at(source, index)
        {
            let Ok(span) = span else {
                return true;
            };
            if has_current_shell_substitution(&source[span.body_start..span.end]) {
                return true;
            }
            index = span.end + 1;
            continue;
        }
        if lexical.advance(source, index, character) {
            index += character.len_utf8();
            continue;
        }
        if lexical.quote != Some('\'') && at(source, index) {
            return true;
        }
        index += character.len_utf8();
    }
    false
}

pub(super) fn at(source: &str, index: usize) -> bool {
    let Some(rest) = source.get(index..).and_then(|rest| rest.strip_prefix("${")) else {
        return false;
    };
    let rest = strip_leading_continuations(rest);
    let rest = rest.strip_prefix('|').unwrap_or(rest);
    let rest = strip_leading_continuations(rest);
    rest.starts_with(char::is_whitespace)
}

fn strip_leading_continuations(mut source: &str) -> &str {
    while let Some(rest) = super::syntax::strip_one_continuation(source) {
        source = rest;
    }
    source
}

#[derive(Default)]
struct LexicalState {
    quote: Option<char>,
    escaped: bool,
    comment: bool,
    ansi_c_quote: Option<()>,
}

impl LexicalState {
    fn can_open_substitution(&self) -> bool {
        !self.escaped && !self.comment && self.quote != Some('\'')
    }

    fn advance(&mut self, source: &str, index: usize, character: char) -> bool {
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
                self.ansi_c_quote = (character == '\'' && super::syntax::ansi_c_quote_opener(source, index)).then_some(());
            } else if self.quote == Some(character) {
                self.ansi_c_quote = None;
            }
            self.quote = super::syntax::updated_quote(self.quote, character);
            return true;
        }
        if self.quote.is_none() && character == '#' && super::syntax::starts_comment(source, index) {
            self.comment = true;
            return true;
        }
        self.quote == Some('\'')
    }
}
