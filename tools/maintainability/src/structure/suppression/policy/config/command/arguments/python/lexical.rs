pub(super) fn normalize_continuations(source: &str) -> String {
    ContinuationScanner::new(source).scan()
}

pub(super) fn executable_code(source: &str) -> String {
    without_literals(source).to_ascii_lowercase()
}

pub(super) fn normalized_qualified_code(source: &str) -> String {
    let executable = executable_code(source);
    let mut normalized = String::with_capacity(executable.len());
    let mut whitespace = String::new();
    for character in executable.chars() {
        if character.is_whitespace() {
            whitespace.push(character);
            continue;
        }
        if character != '.' && !normalized.ends_with('.') {
            normalized.push_str(&whitespace);
        }
        whitespace.clear();
        normalized.push(character);
    }
    normalized.push_str(&whitespace);
    normalized
}

#[cfg(test)]
pub(super) fn has_adjacent_string_literals(source: &str) -> bool {
    let normalized = normalize_continuations(source);
    has_adjacent_literals(&normalized)
}

pub(super) fn has_adjacent_literals(source: &str) -> bool {
    LiteralScanner::new(source).has_adjacent_literals()
}

pub(super) fn has_decoded_escape(source: &str) -> bool {
    LiteralScanner::new(source).has_decoded_escape()
}

pub(super) fn without_literals(source: &str) -> String {
    LiteralScanner::new(source).without_literals()
}

struct ContinuationScanner {
    characters: Vec<char>,
    output: String,
    index: usize,
    delimiter_depth: usize,
    quote: Option<(char, bool)>,
    escaped: bool,
    comment: bool,
}

impl ContinuationScanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            output: String::with_capacity(source.len()),
            index: 0,
            delimiter_depth: 0,
            quote: None,
            escaped: false,
            comment: false,
        }
    }

    fn scan(mut self) -> String {
        while self.index < self.characters.len() {
            if self.comment {
                self.scan_comment();
            } else if self.quote.is_some() {
                self.scan_quoted();
            } else {
                self.scan_code();
            }
            self.index += 1;
        }
        self.output
    }

    fn scan_comment(&mut self) {
        if self.current() == '\n' {
            self.comment = false;
            self.push_line_break();
        }
    }

    fn scan_quoted(&mut self) {
        let (delimiter, triple) = self.quote.expect("quoted scanner state");
        let character = self.current();
        self.output.push(character);
        if self.escaped {
            self.escaped = false;
        } else if character == '\\' {
            self.escaped = true;
        } else if character == delimiter && (!triple || self.followed_by_pair(delimiter)) {
            self.close_quote(delimiter, triple);
        }
    }

    fn scan_code(&mut self) {
        let character = self.current();
        match character {
            '#' => self.comment = true,
            '\'' | '"' => self.open_quote(character),
            '(' | '[' | '{' => {
                self.delimiter_depth += 1;
                self.output.push(character);
            }
            ')' | ']' | '}' => {
                self.delimiter_depth = self.delimiter_depth.saturating_sub(1);
                self.output.push(character);
            }
            '\\' if self.characters.get(self.index + 1) == Some(&'\n') => {
                self.output.push(' ');
                self.index += 1;
            }
            '\\' if self.characters.get(self.index + 1) == Some(&'\r') && self.characters.get(self.index + 2) == Some(&'\n') => {
                self.output.push(' ');
                self.index += 2;
            }
            '\n' => self.push_line_break(),
            '\r' if self.characters.get(self.index + 1) == Some(&'\n') => {}
            _ => self.output.push(character),
        }
    }

    fn open_quote(&mut self, delimiter: char) {
        let triple = self.followed_by_pair(delimiter);
        self.output.push(delimiter);
        if triple {
            self.output.extend([delimiter, delimiter]);
            self.index += 2;
        }
        self.quote = Some((delimiter, triple));
    }

    fn close_quote(&mut self, delimiter: char, triple: bool) {
        if triple {
            self.output.extend([delimiter, delimiter]);
            self.index += 2;
        }
        self.quote = None;
    }

    fn push_line_break(&mut self) {
        self.output.push(if self.delimiter_depth == 0 { '\n' } else { ' ' });
    }

    fn followed_by_pair(&self, character: char) -> bool {
        self.characters.get(self.index + 1) == Some(&character) && self.characters.get(self.index + 2) == Some(&character)
    }

    fn current(&self) -> char {
        self.characters[self.index]
    }
}

struct LiteralScanner {
    characters: Vec<char>,
    index: usize,
}

impl LiteralScanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
        }
    }

    fn has_adjacent_literals(mut self) -> bool {
        while self.index < self.characters.len() {
            let Some(end) = self.literal_end(self.index) else {
                self.index += 1;
                continue;
            };
            let next = self.separator_end(end);
            if self.literal_end(next).is_some() {
                return true;
            }
            self.index = end;
        }
        false
    }

    fn has_decoded_escape(mut self) -> bool {
        while self.index < self.characters.len() {
            let Some(quote) = self.quote_start(self.index) else {
                self.index += 1;
                continue;
            };
            let raw = self.characters[self.index..quote].iter().any(|character| character.eq_ignore_ascii_case(&'r'));
            let end = self.literal_end(self.index).unwrap_or(self.characters.len());
            if !raw && self.characters[quote..end].contains(&'\\') {
                return true;
            }
            self.index = end;
        }
        false
    }

    fn without_literals(mut self) -> String {
        let mut executable = String::with_capacity(self.characters.len());
        while self.index < self.characters.len() {
            if let Some(end) = self.literal_end(self.index) {
                executable.push(' ');
                self.index = end;
            } else {
                executable.push(self.characters[self.index]);
                self.index += 1;
            }
        }
        executable
    }

    fn literal_end(&self, start: usize) -> Option<usize> {
        let quote = self.quote_start(start)?;
        let delimiter = self.characters[quote];
        let triple = self.characters.get(quote + 1) == Some(&delimiter) && self.characters.get(quote + 2) == Some(&delimiter);
        let quote_width = if triple { 3 } else { 1 };
        let mut index = quote + quote_width;
        while index < self.characters.len() {
            if self.characters[index] == '\\' {
                index = index.saturating_add(2);
            } else if self.characters[index] == delimiter && (!triple || self.characters.get(index + 1) == Some(&delimiter) && self.characters.get(index + 2) == Some(&delimiter)) {
                return Some(index + quote_width);
            } else {
                index += 1;
            }
        }
        Some(self.characters.len())
    }

    fn quote_start(&self, start: usize) -> Option<usize> {
        let character = *self.characters.get(start)?;
        if matches!(character, '\'' | '"') {
            return Some(start);
        }
        if start > 0 && super::is_identifier_character(self.characters[start - 1]) {
            return None;
        }
        let mut end = start;
        while end < self.characters.len() && end - start < 3 && matches!(self.characters[end].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
            end += 1;
        }
        (end > start && matches!(self.characters.get(end), Some('\'' | '"'))).then_some(end)
    }

    fn separator_end(&self, mut index: usize) -> usize {
        loop {
            while matches!(self.characters.get(index), Some(' ' | '\t' | '\r')) {
                index += 1;
            }
            if self.characters.get(index) == Some(&'\\') && self.characters.get(index + 1) == Some(&'\n') {
                index += 2;
                continue;
            }
            return index;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::has_adjacent_string_literals;

    #[test]
    fn adjacent_literals_are_detected_only_within_one_python_expression() {
        assert!(has_adjacent_string_literals("subprocess.run([\"cargo\", \"clippy\", \"--\", \"-\" \"A\", \"warnings\"])\n"));
        assert!(has_adjacent_string_literals("VALUES = (r\"cargo\"  f\" clippy\")\n"));
        assert!(has_adjacent_string_literals("VALUE = \"cargo\" \\\n    \" clippy\"\n"));
        assert!(!has_adjacent_string_literals("\"module doc\"\n\"second statement\"\n"));
        assert!(!has_adjacent_string_literals("subprocess.run([\"cargo\", \"clippy\"])\n"));
        assert!(!has_adjacent_string_literals("identifier\"invalid but not concatenated\"\n"));
    }
}
