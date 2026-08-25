use super::lexical;

pub(in super::super) struct Span {
    pub(in super::super) body_start: usize,
    pub(in super::super) end: usize,
}

pub(super) fn process_span_at(source: &str, index: usize) -> Option<Result<Span, ()>> {
    let opener = matches!(source.as_bytes().get(index), Some(b'<' | b'>')) && source.as_bytes().get(index + 1) == Some(&b'(');
    opener.then(|| {
        let body_start = index + 2;
        parenthesized_end(source, body_start, false).map(|end| Span { body_start, end }).ok_or(())
    })
}

pub(in super::super) fn span_at(source: &str, index: usize) -> Option<Result<Span, ()>> {
    if source[index..].starts_with("$(") {
        let body_start = index + 2;
        let arithmetic = source.as_bytes().get(body_start) == Some(&b'(');
        return Some(parenthesized_end(source, body_start, arithmetic).map(|end| Span { body_start, end }).ok_or(()));
    }
    (source.as_bytes().get(index) == Some(&b'`')).then(|| {
        let body_start = index + 1;
        backtick_end(source, body_start).map(|end| Span { body_start, end }).ok_or(())
    })
}

fn parenthesized_end(source: &str, mut index: usize, arithmetic: bool) -> Option<usize> {
    let mut depth = 1_usize;
    let mut lexical = lexical::State::default();
    while index < source.len() {
        let character = source[index..].chars().next()?;
        if !arithmetic && lexical.active_continuation_len(source, index).is_some() {
            return None;
        }
        if lexical.can_open_substitution()
            && let Some(span) = span_at(source, index)
        {
            let span = span.ok()?;
            index = span.end + 1;
            continue;
        }
        if !arithmetic && lexical.structural_syntax_is_active() && (case_opener_at(source, index) || heredoc_opener_at(source, index)) {
            return None;
        }
        if lexical.advance(source, index, character) {
            index += character.len_utf8();
            continue;
        }
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += character.len_utf8();
    }
    None
}

fn backtick_end(source: &str, mut index: usize) -> Option<usize> {
    let mut escaped = false;
    while index < source.len() {
        let character = source[index..].chars().next()?;
        if continuation_after_backslash_run(source, index, character) {
            return None;
        }
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '`' {
            return Some(index);
        }
        index += character.len_utf8();
    }
    None
}

fn continuation_after_backslash_run(source: &str, index: usize, character: char) -> bool {
    let newline = character == '\n' || character == '\r' && source[index..].starts_with("\r\n");
    newline && source[..index].ends_with('\\')
}

fn case_opener_at(source: &str, index: usize) -> bool {
    let rest = &source[index..];
    rest.starts_with("case") && !source[..index].chars().next_back().is_some_and(shell_name_character) && !rest[4..].chars().next().is_some_and(shell_name_character)
}

fn heredoc_opener_at(source: &str, index: usize) -> bool {
    source[index..].starts_with("<<") && !source[index..].starts_with("<<<")
}

const fn shell_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

#[cfg(test)]
mod tests {
    use super::span_at;

    #[test]
    fn nested_quotes_do_not_close_the_parent_substitution() {
        for source in [r#"$(printf '%s' '"') trailing"#, r#"$(printf '%s' "$(printf '%s' '"')") trailing"#] {
            let span = span_at(source, 0).expect("opener").expect("balanced substitution");
            assert_eq!(&source[span.end..], ") trailing");
        }
    }

    #[test]
    fn backticks_have_an_independent_quote_frame() {
        let source = r#"`printf '%s' '"'` trailing"#;
        let span = span_at(source, 0).expect("opener").expect("balanced substitution");
        assert_eq!(&source[span.end..], "` trailing");
    }

    #[test]
    fn malformed_substitutions_fail_closed() {
        assert!(span_at("$(printf", 0).expect("opener").is_err());
        for source in [
            "$(printf '%s' \\\n'safe')",
            "$(printf '%s' \\\r\n'safe')",
            "$(ca\\\nse x in x) printf safe ;; esac)",
            "$(ca\\\r\nse x in x) printf safe ;; esac)",
            "$(cat <\\\n<'DOC'\nsafe\nDOC\n)",
        ] {
            assert!(span_at(source, 0).expect("opener").is_err(), "{source:?}");
        }
        assert!(span_at("`printf", 0).expect("opener").is_err());
        for source in [
            "`cat <\\\n(sh quality/lint.txt)`",
            "`cat <\\\r\n(sh quality/lint.txt)`",
            "`cat <\\\\\n(sh quality/lint.txt)`",
            "`cat <\\\\\\\r\n(sh quality/lint.txt)`",
            "$(printf '%s' `cat <\\\n(sh quality/lint.txt)`)",
            "$(printf '%s' `cat <\\\\\n(sh quality/lint.txt)`)",
        ] {
            assert!(span_at(source, 0).expect("opener").is_err(), "{source:?}");
        }
    }

    #[test]
    fn unsupported_nested_shell_grammar_fails_closed() {
        for source in ["$(case x in x) printf safe ;; esac)", "$(cat <<'DOC'\n)\nDOC\n)"] {
            assert!(span_at(source, 0).expect("opener").is_err(), "{source}");
        }
    }

    #[test]
    fn simple_and_arithmetic_substitutions_remain_balanced() {
        for source in [
            "$(printf '%s' safe) trailing",
            "$(printf '%s' showcase) trailing",
            "$((1 << 2)) trailing",
            "`printf '%s' safe` trailing",
        ] {
            assert!(span_at(source, 0).expect("opener").is_ok(), "{source}");
        }
    }
}
