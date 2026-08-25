use std::collections::VecDeque;

#[derive(Clone)]
pub(super) struct Document {
    pub(super) delimiter: String,
    pub(super) strip_tabs: bool,
    pub(super) allows_expansion: bool,
}

pub(super) enum Line<'a> {
    Command(&'a str),
    Body { text: &'a str, document: Document, delimiter: bool },
}

pub(super) struct Lines<'a> {
    source: std::str::Lines<'a>,
    scan: Scan,
    declared: VecDeque<Document>,
    pending: VecDeque<Document>,
}

impl<'a> Lines<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self {
            source: source.lines(),
            scan: Scan::default(),
            declared: VecDeque::new(),
            pending: VecDeque::new(),
        }
    }

    pub(super) const fn has_opaque_delimiter(&self) -> bool {
        self.scan.has_opaque_delimiter()
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = Line<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.source.next()?;
        if let Some(document) = self.pending.front().cloned() {
            let candidate = if document.strip_tabs { line.trim_start_matches('\t') } else { line };
            let delimiter = candidate == document.delimiter;
            if delimiter {
                self.pending.pop_front();
            }
            return Some(Line::Body { text: line, document, delimiter });
        }
        self.declared.extend(self.scan.documents(line));
        if !self.scan.command_continues() {
            self.pending.append(&mut self.declared);
        }
        Some(Line::Command(line))
    }
}

pub(super) fn command_source(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    for event in Lines::new(source) {
        if let Line::Command(line) = event {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct Scan {
    shell_quote: Option<char>,
    shell_ansi_c_quote: Option<()>,
    arithmetic_quote: Option<char>,
    arithmetic_depth: usize,
    opaque_delimiter: bool,
    line_continues: bool,
    continued_less_than: bool,
}

impl Scan {
    pub(super) fn documents(&mut self, line: &str) -> Vec<Document> {
        let continued_less_than = std::mem::take(&mut self.continued_less_than);
        if continued_less_than && line.starts_with('<') {
            self.opaque_delimiter = true;
        }
        let characters = line.chars().collect::<Vec<_>>();
        let mut documents = Vec::new();
        let mut index = 0;
        let mut escaped = false;
        let mut trailing_operator = String::new();
        let mut active_previous = None;
        while index < characters.len() {
            let character = characters[index];
            if escaped {
                escaped = false;
                trailing_operator.clear();
                active_previous = None;
            } else if self.arithmetic_depth > 0 {
                self.update_arithmetic(character, &mut escaped);
                trailing_operator.clear();
                active_previous = None;
            } else if character == '\\' && (self.shell_quote != Some('\'') || self.shell_ansi_c_quote.is_some()) {
                escaped = true;
                trailing_operator.clear();
            } else if matches!(character, '\'' | '"') {
                self.update_shell_quote(&characters, index, character);
                trailing_operator.clear();
                active_previous = None;
            } else if comment_opener(&characters, index, self.shell_quote) {
                break;
            } else if arithmetic_opener(&characters, index, self.shell_quote) {
                self.arithmetic_depth = 2;
                self.arithmetic_quote = None;
                trailing_operator.clear();
                active_previous = None;
                index += 2 + usize::from(character == '$');
                continue;
            } else if heredoc_opener(&characters, index, self.shell_quote) {
                let (document, end) = document_after_opener(&characters, index);
                match document {
                    Some(document) => documents.push(document),
                    None => self.opaque_delimiter = true,
                }
                trailing_operator.clear();
                active_previous = None;
                index = end;
                continue;
            } else if self.shell_quote.is_none() && matches!(character, '&' | '|') {
                trailing_operator.push(character);
                active_previous = None;
            } else if !character.is_whitespace() {
                trailing_operator.clear();
                active_previous = (character == '<').then_some(character);
            } else {
                active_previous = None;
            }
            index += 1;
        }
        self.line_continues = self.shell_quote.is_some() || self.arithmetic_depth > 0 || escaped || continuation_operator(&trailing_operator);
        self.continued_less_than = escaped && self.shell_quote.is_none() && self.arithmetic_depth == 0 && (continued_less_than && line == "\\" || active_previous == Some('<'));
        documents
    }

    pub(super) const fn has_opaque_delimiter(&self) -> bool {
        self.opaque_delimiter
    }

    fn update_shell_quote(&mut self, characters: &[char], index: usize, character: char) {
        if self.shell_quote.is_none() {
            self.shell_ansi_c_quote = (character == '\'' && ansi_c_quote_opener(characters, index)).then_some(());
        } else if self.shell_quote == Some(character) {
            self.shell_ansi_c_quote = None;
        }
        self.shell_quote = updated_quote(self.shell_quote, character);
    }

    const fn command_continues(&self) -> bool {
        self.line_continues
    }

    fn update_arithmetic(&mut self, character: char, escaped: &mut bool) {
        if character == '\\' && self.arithmetic_quote != Some('\'') {
            *escaped = true;
            return;
        }
        if matches!(character, '\'' | '"') {
            self.arithmetic_quote = updated_quote(self.arithmetic_quote, character);
            return;
        }
        if self.arithmetic_quote.is_some() {
            return;
        }
        match character {
            '(' => self.arithmetic_depth += 1,
            ')' => self.arithmetic_depth = self.arithmetic_depth.saturating_sub(1),
            _ => {}
        }
    }
}

fn ansi_c_quote_opener(characters: &[char], quote_index: usize) -> bool {
    let Some(dollar_index) = quote_index.checked_sub(1).filter(|index| characters.get(*index) == Some(&'$')) else {
        return false;
    };
    characters[..dollar_index]
        .iter()
        .rev()
        .take_while(|character| **character == '\\')
        .count()
        .is_multiple_of(2)
}

fn continuation_operator(operator: &str) -> bool {
    matches!(operator, "&&" | "||" | "|&" | "|")
}

fn comment_opener(characters: &[char], index: usize, quote: Option<char>) -> bool {
    quote.is_none()
        && characters[index] == '#'
        && index
            .checked_sub(1)
            .is_none_or(|previous| characters[previous].is_whitespace() || matches!(characters[previous], ';' | '&' | '|' | '(' | ')'))
}

fn document_after_opener(characters: &[char], start: usize) -> (Option<Document>, usize) {
    let mut index = start + 2;
    let strip_tabs = characters.get(index) == Some(&'-');
    index += usize::from(strip_tabs);
    while characters.get(index).is_some_and(|character| character.is_whitespace()) {
        index += 1;
    }
    let Some((delimiter, end, allows_expansion)) = delimiter(characters, index) else {
        return (None, index + 1);
    };
    (
        Some(Document {
            delimiter,
            strip_tabs,
            allows_expansion,
        }),
        end,
    )
}

fn heredoc_opener(characters: &[char], index: usize, quote: Option<char>) -> bool {
    quote.is_none()
        && characters.get(index..index + 2) == Some(&['<', '<'])
        && characters.get(index + 2) != Some(&'<')
        && index.checked_sub(1).is_none_or(|previous| characters[previous] != '<')
}

fn arithmetic_opener(characters: &[char], index: usize, quote: Option<char>) -> bool {
    let unquoted = quote.is_none() && characters.get(index..index + 2) == Some(&['(', '(']);
    let expansion = quote != Some('\'') && characters.get(index..index + 3) == Some(&['$', '(', '(']);
    unquoted || expansion
}

fn updated_quote(quote: Option<char>, character: char) -> Option<char> {
    if quote == Some(character) { None } else { quote.or(Some(character)) }
}

fn delimiter(characters: &[char], start: usize) -> Option<(String, usize, bool)> {
    let mut index = start;
    let mut quote = None;
    let mut value = String::new();
    let mut escaped = false;
    let mut quoted = false;
    while let Some(character) = characters.get(index).copied() {
        if quote.is_none() && character == '$' && matches!(characters.get(index + 1), Some('\'' | '"')) {
            return None;
        }
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            quoted = true;
            if quote != Some('"') || matches!(characters.get(index + 1), Some('$' | '`' | '"' | '\\')) {
                escaped = true;
            } else {
                value.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
                quoted = true;
            } else if quote.is_none() {
                quote = Some(character);
                quoted = true;
            } else {
                value.push(character);
            }
        } else if quote.is_none() && (character.is_whitespace() || matches!(character, ';' | '|' | '&' | '<' | '>' | '(' | ')')) {
            break;
        } else {
            value.push(character);
        }
        index += 1;
    }
    (quote.is_none() && !escaped && (!value.is_empty() || quoted)).then_some((value, index, !quoted))
}
