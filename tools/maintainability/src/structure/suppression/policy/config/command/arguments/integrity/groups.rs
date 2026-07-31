use std::collections::BTreeSet;

pub(super) fn masked_quality_group(source: &str, case_insensitive_tools: bool, quality_functions: &BTreeSet<String>) -> bool {
    Scanner::new(source, case_insensitive_tools, quality_functions).has_masked_group()
}

struct Scanner<'a> {
    source: &'a str,
    case_insensitive_tools: bool,
    quality_functions: &'a BTreeSet<String>,
    groups: Vec<(char, usize)>,
    cases: Vec<usize>,
    word_start: Option<usize>,
    command_start: bool,
    quote: Option<char>,
    lexical_mode: LexicalMode,
}

impl<'a> Scanner<'a> {
    const fn new(source: &'a str, case_insensitive_tools: bool, quality_functions: &'a BTreeSet<String>) -> Self {
        Self {
            source,
            case_insensitive_tools,
            quality_functions,
            groups: Vec::new(),
            cases: Vec::new(),
            word_start: None,
            command_start: true,
            quote: None,
            lexical_mode: LexicalMode::Plain,
        }
    }

    fn has_masked_group(mut self) -> bool {
        for (index, character) in self.source.char_indices() {
            if self.collects_word(index, character) {
                continue;
            }
            if self.word_end_masks_group(index, character) {
                return true;
            }
            if self.advance_lexical_state(index, character) {
                continue;
            }
            if matches!(character, '\n' | ';' | '|' | '&' | '{' | '(' | ')') {
                self.command_start = true;
            }
            if matches!(character, '{' | '(') {
                self.groups.push((character, index));
                continue;
            }
            let Some(open) = matching_open(character) else {
                continue;
            };
            let Some((_, open_index)) = self.groups.last().copied().filter(|(delimiter, _)| *delimiter == open) else {
                continue;
            };
            self.groups.pop();
            let body = &self.source[open_index + open.len_utf8()..index];
            if self.contains_quality(body) && group_context_masks_failure(self.source, open_index, index + character.len_utf8()) {
                return true;
            }
        }
        self.finish_word(self.source.len())
    }

    fn collects_word(&mut self, index: usize, character: char) -> bool {
        let collects = self.quote.is_none() && self.lexical_mode == LexicalMode::Plain && is_shell_word_character(character);
        if collects {
            self.word_start.get_or_insert(index);
        }
        collects
    }

    fn word_end_masks_group(&mut self, index: usize, character: char) -> bool {
        self.quote.is_none() && self.lexical_mode == LexicalMode::Plain && !is_shell_word_character(character) && self.finish_word(index)
    }

    fn finish_word(&mut self, end: usize) -> bool {
        let Some(start) = self.word_start.take() else {
            return false;
        };
        let word = &self.source[start..end];
        let starts_command = self.command_start;
        if starts_command && word == "case" {
            self.cases.push(start);
        }
        let masked = starts_command
            && word == "esac"
            && self
                .cases
                .pop()
                .is_some_and(|open| self.contains_quality(&self.source[open + "case".len()..start]) && group_context_masks_failure(self.source, open, end));
        self.command_start = matches!(word, "do" | "elif" | "else" | "if" | "then" | "until" | "while");
        masked
    }

    fn advance_lexical_state(&mut self, index: usize, character: char) -> bool {
        if self.lexical_mode == LexicalMode::Comment {
            if character == '\n' {
                self.lexical_mode = LexicalMode::Plain;
                self.command_start = true;
            }
            return true;
        }
        if self.lexical_mode == LexicalMode::Escaped {
            self.lexical_mode = LexicalMode::Plain;
            return true;
        }
        if character == '\\' && self.quote != Some('\'') {
            self.lexical_mode = LexicalMode::Escaped;
            return true;
        }
        if matches!(character, '\'' | '"' | '`') {
            self.quote = updated_quote(self.quote, character);
            return true;
        }
        if self.quote.is_none() && character == '#' && starts_comment(self.source, index) {
            self.lexical_mode = LexicalMode::Comment;
            return true;
        }
        self.quote.is_some()
    }

    fn contains_quality(&self, body: &str) -> bool {
        super::contains_quality_or_function_command(body, self.case_insensitive_tools, self.quality_functions)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LexicalMode {
    Plain,
    Escaped,
    Comment,
}

const fn is_shell_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

const fn matching_open(character: char) -> Option<char> {
    match character {
        '}' => Some('{'),
        ')' => Some('('),
        _ => None,
    }
}

fn group_context_masks_failure(source: &str, open: usize, after_close: usize) -> bool {
    suffix_masks_failure(&source[after_close..]) || prefix_inverts_status(&source[..open])
}

fn suffix_masks_failure(suffix: &str) -> bool {
    let suffix = suffix.trim_start();
    if suffix.starts_with('|') || suffix.starts_with("&&") {
        return true;
    }
    if suffix.starts_with('&') && !suffix.starts_with("&>") {
        return true;
    }
    let control = suffix.strip_prefix(';').map_or(suffix, str::trim_start);
    starts_control_word(control, "then") || starts_control_word(control, "do")
}

fn prefix_inverts_status(prefix: &str) -> bool {
    let command_start = prefix.rfind(['\n', ';', '|', '&']).map_or(0, |index| index + 1);
    let command = prefix[command_start..].trim();
    command.split_whitespace().next_back() == Some("!")
}

fn starts_control_word(source: &str, word: &str) -> bool {
    source
        .strip_prefix(word)
        .is_some_and(|rest| rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace))
}

fn starts_comment(source: &str, index: usize) -> bool {
    source[..index]
        .chars()
        .next_back()
        .is_none_or(|previous| previous.is_whitespace() || matches!(previous, ';' | '|' | '&' | '(' | ')' | '{' | '}'))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::masked_quality_group;

    fn masks(source: &str) -> bool {
        masked_quality_group(source, true, &BTreeSet::new())
    }

    #[test]
    fn multiline_compound_commands_cannot_hide_quality_failures() {
        assert!(masks("{\n just check-quality\n true\n} || true"));
        assert!(masks("(\n cargo test --locked\n true\n) && echo accepted"));
        assert!(masks("while (\n cargo clippy --locked\n true\n); do echo retrying; done"));
        assert!(masks("if (\n cargo test --locked\n true\n)\nthen echo accepted; fi"));
        assert!(masks("! {\n just check-quality\n true\n}"));
        assert!(masks("if ! {\n just check-quality\n true\n}; then echo rejected; fi"));
        assert!(masks("case \"$mode\" in\n checked)\n just check-quality; true ;;\nesac || true"));
        assert!(masks("if case \"$mode\" in\n checked)\n cargo test --locked ;;\nesac; then echo accepted; fi"));
    }

    #[test]
    fn unmasked_and_inert_groups_are_allowed() {
        assert!(!masks("{\n just check-quality\n}"));
        assert!(!masks("(\n cargo test --locked\n)"));
        assert!(!masks("printf '%s\n' '{ just check-quality; } || true'"));
        assert!(!masks("# { just check-quality; } || true\nprintf accepted"));
        assert!(!masks("case \"$mode\" in\n checked)\n just check-quality ;;\nesac"));
        assert!(!masks("printf '%s\n' 'case mode in checked) just check-quality ;; esac || true'"));
    }
}
