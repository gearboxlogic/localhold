pub(super) struct Document {
    pub(super) delimiter: String,
    pub(super) strip_tabs: bool,
    pub(super) allows_expansion: bool,
}

#[derive(Default)]
pub(super) struct Scan {
    shell_quote: Option<char>,
    arithmetic_quote: Option<char>,
    arithmetic_depth: usize,
}

impl Scan {
    pub(super) fn documents(&mut self, line: &str) -> Vec<Document> {
        let characters = line.chars().collect::<Vec<_>>();
        let mut documents = Vec::new();
        let mut index = 0;
        let mut escaped = false;
        while index < characters.len() {
            let character = characters[index];
            if escaped {
                escaped = false;
            } else if self.arithmetic_depth > 0 {
                self.update_arithmetic(character, &mut escaped);
            } else if character == '\\' && self.shell_quote != Some('\'') {
                escaped = true;
            } else if matches!(character, '\'' | '"') {
                self.shell_quote = updated_quote(self.shell_quote, character);
            } else if arithmetic_opener(&characters, index, self.shell_quote) {
                self.arithmetic_depth = 2;
                self.arithmetic_quote = None;
                index += 2 + usize::from(character == '$');
                continue;
            } else if heredoc_opener(&characters, index, self.shell_quote) {
                let (document, end) = document_after_opener(&characters, index);
                documents.extend(document);
                index = end;
                continue;
            }
            index += 1;
        }
        documents
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
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
            quoted = true;
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
    (quote.is_none() && !escaped && !value.is_empty()).then_some((value, index, !quoted))
}
