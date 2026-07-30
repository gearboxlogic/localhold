pub(super) fn join_implicit_continuations(source: &str) -> String {
    Scanner::new(source).scan()
}

struct Scanner {
    characters: Vec<char>,
    output: String,
    index: usize,
    delimiter_depth: usize,
    quote: Option<(char, bool)>,
    escaped: bool,
    comment: bool,
}

impl Scanner {
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
