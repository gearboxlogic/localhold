#[derive(Default)]
pub(super) struct State {
    quote: Option<char>,
    escaped: bool,
    comment: bool,
    ansi_c_quote: Option<()>,
}

impl State {
    pub(super) fn can_open_substitution(&self) -> bool {
        !self.escaped && !self.comment && self.quote != Some('\'')
    }

    pub(super) const fn can_open_process_substitution(&self) -> bool {
        !self.escaped && !self.comment && self.quote.is_none()
    }

    pub(super) const fn structural_syntax_is_active(&self) -> bool {
        !self.escaped && !self.comment && self.quote.is_none()
    }

    pub(super) fn active_continuation_len(&self, source: &str, index: usize) -> Option<usize> {
        if self.escaped || self.quote == Some('\'') && self.ansi_c_quote.is_none() || source.as_bytes().get(index) != Some(&b'\\') {
            return None;
        }
        source[index + 1..]
            .strip_prefix("\r\n")
            .map(|_| 3)
            .or_else(|| source[index + 1..].strip_prefix('\n').map(|_| 2))
    }

    pub(super) fn advance(&mut self, source: &str, index: usize, character: char) -> bool {
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
            self.update_quote(source, index, character);
            return true;
        }
        if self.quote.is_none() && character == '#' && comment_opener_at(source, index) {
            self.comment = true;
            return true;
        }
        self.quote.is_some()
    }

    fn update_quote(&mut self, source: &str, index: usize, character: char) {
        if self.quote.is_none() {
            self.ansi_c_quote = (character == '\'' && ansi_c_quote_opener(source, index)).then_some(());
        } else if self.quote == Some(character) {
            self.ansi_c_quote = None;
        }
        self.quote = if self.quote == Some(character) { None } else { self.quote.or(Some(character)) };
    }
}

pub(in super::super) fn without_active_continuations(source: &str) -> Result<String, ()> {
    let mut normalized = source.to_owned();
    loop {
        let next = normalize_once(&normalized)?;
        if next.len() == normalized.len() {
            return Ok(next);
        }
        debug_assert!(next.len() < normalized.len());
        normalized = next;
    }
}

fn normalize_once(source: &str) -> Result<String, ()> {
    let mut output = String::with_capacity(source.len());
    let mut lexical = State::default();
    let mut index = 0;
    while index < source.len() {
        let Some(character) = source[index..].chars().next() else {
            break;
        };
        if lexical.can_open_substitution()
            && let Some(span) = substitution_span_at(source, index, lexical.can_open_process_substitution())
        {
            let span = span?;
            output.push_str(&source[index..=span.end]);
            index = span.end + 1;
            continue;
        }
        if let Some(length) = lexical.active_continuation_len(source, index) {
            index += length;
            continue;
        }
        lexical.advance(source, index, character);
        output.push(character);
        index += character.len_utf8();
    }
    Ok(output)
}

fn substitution_span_at(source: &str, index: usize, process_is_active: bool) -> Option<Result<super::span::Span, ()>> {
    super::span::span_at(source, index).or_else(|| process_is_active.then(|| super::span::process_span_at(source, index)).flatten())
}

fn ansi_c_quote_opener(source: &str, quote_index: usize) -> bool {
    let Some(dollar_index) = quote_index.checked_sub(1).filter(|index| source.as_bytes().get(*index) == Some(&b'$')) else {
        return false;
    };
    source[..dollar_index].bytes().rev().take_while(|byte| *byte == b'\\').count().is_multiple_of(2)
}

fn comment_opener_at(source: &str, index: usize) -> bool {
    source[..index]
        .chars()
        .next_back()
        .is_none_or(|character| character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')'))
}

#[cfg(test)]
mod tests {
    use super::without_active_continuations;

    #[test]
    fn only_active_shell_continuations_are_removed() {
        assert_eq!(without_active_continuations("<\\\n(func)"), Ok("<(func)".to_owned()));
        assert_eq!(without_active_continuations(">\\\r\n(func)"), Ok(">(func)".to_owned()));
        assert_eq!(without_active_continuations("'<\\\n(func)'"), Ok("'<\\\n(func)'".to_owned()));
        assert_eq!(without_active_continuations("'`cat <\\\n(func)`'"), Ok("'`cat <\\\n(func)`'".to_owned()));
        assert_eq!(without_active_continuations("<\\\\\n(func)"), Ok("<\\\\\n(func)".to_owned()));
    }

    #[test]
    fn nested_substitutions_have_independent_quote_frames() {
        for source in [
            r#"printf "%s" "$(printf '%s' '"')"; <\
(func)"#,
            r#"printf "%s" "`printf '%s' '"'`"; <\
(func)"#,
            r#"printf "%s" "$(printf '%s' "$(printf '%s' '"')")"; >\
(func)"#,
        ] {
            let normalized = without_active_continuations(source).expect("balanced substitutions");
            assert_eq!(normalized, source.replace("\\\r\n", "").replace("\\\n", ""));
        }
    }

    #[test]
    fn continuation_formed_substitutions_are_rescanned() {
        for source in [
            "printf '%s' \"$\\\n(printf '%s' '\"')\"; <\\\n(func)",
            "printf '%s' \"$\\\r\n(printf '%s' '\"')\"; >\\\r\n(func)",
            "printf '%s' \"$\\\n\\\n(printf '%s' '\"')\"; <\\\n(func)",
            "printf '%s' $\\\n'x\\''; <\\\n(func)",
        ] {
            let normalized = without_active_continuations(source).expect("balanced substitutions");
            assert_eq!(normalized, source.replace("\\\r\n", "").replace("\\\n", ""));
        }
    }

    #[test]
    fn malformed_substitutions_make_normalization_opaque() {
        assert_eq!(without_active_continuations("printf '%s' \"$(printf\"; <\\\n(func)"), Err(()));
        assert_eq!(without_active_continuations("printf '%s' `printf; <\\\n(func)"), Err(()));
        assert_eq!(without_active_continuations("printf '%s' \"$\\\n(printf\""), Err(()));
        assert_eq!(without_active_continuations("values=(<\\\n(func"), Err(()));
        for source in [
            "value=`cat <\\\n(sh quality/lint.txt)`",
            "value=`cat <\\\r\n(sh quality/lint.txt)`",
            "value=`cat <\\\\\n(sh quality/lint.txt)`",
            "value=`cat <\\\\\\\r\n(sh quality/lint.txt)`",
            "value=$(printf '%s' `cat <\\\n(sh quality/lint.txt)`)",
            "value=$(printf '%s' `cat <\\\\\n(sh quality/lint.txt)`)",
        ] {
            assert_eq!(without_active_continuations(source), Err(()), "{source:?}");
        }
    }
}
