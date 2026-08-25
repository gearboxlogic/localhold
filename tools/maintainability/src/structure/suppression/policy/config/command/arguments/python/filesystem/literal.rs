pub(super) fn static_string(expression: &str) -> Option<String> {
    let mut parser = Parser::new(expression.trim());
    let mut value = parser.literal()?;
    loop {
        parser.skip_whitespace();
        if parser.at_end() {
            return Some(value);
        }
        if parser.take('+') {
            parser.skip_whitespace();
        }
        let adjacent = parser.literal()?;
        value.push_str(&adjacent);
    }
}

struct Parser {
    characters: Vec<char>,
    index: usize,
}

impl Parser {
    fn new(expression: &str) -> Self {
        Self {
            characters: expression.chars().collect(),
            index: 0,
        }
    }

    fn literal(&mut self) -> Option<String> {
        let prefix_start = self.index;
        while self
            .characters
            .get(self.index)
            .is_some_and(|character| matches!(character.to_ascii_lowercase(), 'b' | 'r' | 'u'))
        {
            self.index += 1;
        }
        let prefix = self.characters[prefix_start..self.index].iter().collect::<String>();
        if prefix.len() > 2 || !valid_prefix(&prefix) {
            return None;
        }
        let delimiter = *self.characters.get(self.index).filter(|character| matches!(character, '\'' | '"'))?;
        let raw = prefix.to_ascii_lowercase().contains('r');
        let triple = self.characters.get(self.index + 1) == Some(&delimiter) && self.characters.get(self.index + 2) == Some(&delimiter);
        let width = if triple { 3 } else { 1 };
        self.index += width;
        let mut value = String::new();
        while self.index < self.characters.len() {
            if self.characters[self.index] == delimiter
                && (!triple || self.characters.get(self.index + 1) == Some(&delimiter) && self.characters.get(self.index + 2) == Some(&delimiter))
            {
                self.index += width;
                return Some(value);
            }
            let character = self.characters[self.index];
            self.index += 1;
            if character != '\\' || raw {
                value.push(character);
                continue;
            }
            let escaped = *self.characters.get(self.index)?;
            self.index += 1;
            if !matches!(escaped, '\\' | '/' | '\'' | '"') {
                return None;
            }
            value.push(escaped);
        }
        None
    }

    fn skip_whitespace(&mut self) {
        while self.characters.get(self.index).is_some_and(|character| character.is_whitespace()) {
            self.index += 1;
        }
    }

    fn take(&mut self, expected: char) -> bool {
        if self.characters.get(self.index) != Some(&expected) {
            return false;
        }
        self.index += 1;
        true
    }

    const fn at_end(&self) -> bool {
        self.index == self.characters.len()
    }
}

fn valid_prefix(prefix: &str) -> bool {
    matches!(prefix.to_ascii_lowercase().as_str(), "" | "b" | "br" | "r" | "rb" | "u")
}

#[cfg(test)]
mod tests {
    use super::static_string;

    #[test]
    fn evaluates_only_static_literal_concatenation() {
        assert_eq!(static_string("'target/' 'report.txt'"), Some("target/report.txt".to_owned()));
        assert_eq!(static_string("r'target/' + 'report.txt'"), Some("target/report.txt".to_owned()));
        assert_eq!(static_string("'''target/''' + b'report.txt'"), Some("target/report.txt".to_owned()));
        assert_eq!(static_string("'target/' + name"), None);
        assert_eq!(static_string("name + 'report.txt'"), None);
        assert_eq!(static_string("'target/'.format()"), None);
        assert_eq!(static_string("f'target/{name}'"), None);
        assert_eq!(static_string("('target/report.txt')"), None);
    }
}
