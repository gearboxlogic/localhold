pub(super) fn has_dynamic_code(source: &str) -> bool {
    Scanner::new(source).has_dynamic_code()
}

struct Scanner {
    characters: Vec<char>,
    index: usize,
}

impl Scanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
        }
    }

    fn has_dynamic_code(mut self) -> bool {
        while self.index < self.characters.len() {
            if self.advance() {
                return true;
            }
        }
        false
    }

    fn advance(&mut self) -> bool {
        if self.characters[self.index] == '#' {
            self.skip_comment();
            return false;
        }
        if let Some(literal) = self.string_literal(self.index) {
            return self.advance_string(literal);
        }
        if is_identifier_start(self.characters[self.index]) {
            return self.advance_identifier();
        }
        self.index += 1;
        false
    }

    fn advance_string(&mut self, literal: StringLiteral) -> bool {
        self.index = literal.end;
        literal.formatted && self.f_string_has_dynamic_code(literal.content_start, literal.content_end)
    }

    fn advance_identifier(&mut self) -> bool {
        let (path, end) = self.dotted_identifier(self.index);
        self.index = end;
        dynamic_path(&path)
    }

    fn skip_comment(&mut self) {
        while self.characters.get(self.index).is_some_and(|character| *character != '\n') {
            self.index += 1;
        }
    }

    fn dotted_identifier(&self, start: usize) -> (Vec<String>, usize) {
        let mut path = Vec::new();
        let mut index = start;
        loop {
            let end = self.identifier_end(index);
            path.push(self.characters[index..end].iter().collect());
            index = self.skip_whitespace(end);
            if self.characters.get(index) != Some(&'.') {
                return (path, end);
            }
            let next = self.skip_whitespace(index + 1);
            if !self.characters.get(next).is_some_and(|character| is_identifier_start(*character)) {
                return (path, end);
            }
            index = next;
        }
    }

    fn identifier_end(&self, mut index: usize) -> usize {
        index += 1;
        while self.characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
            index += 1;
        }
        index
    }

    fn string_literal(&self, start: usize) -> Option<StringLiteral> {
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
            return None;
        }
        let mut quote = start;
        let mut formatted = false;
        while quote < self.characters.len() && quote - start < 3 && matches!(self.characters[quote].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
            formatted |= self.characters[quote].eq_ignore_ascii_case(&'f');
            quote += 1;
        }
        let delimiter = *self.characters.get(quote).filter(|character| matches!(character, '\'' | '"'))?;
        let triple = self.characters.get(quote + 1) == Some(&delimiter) && self.characters.get(quote + 2) == Some(&delimiter);
        let width = if triple { 3 } else { 1 };
        let mut index = quote + width;
        while index < self.characters.len() {
            if self.characters[index] == '\\' {
                index = index.saturating_add(2);
            } else if self.characters[index] == delimiter && (!triple || self.characters.get(index + 1) == Some(&delimiter) && self.characters.get(index + 2) == Some(&delimiter)) {
                return Some(StringLiteral {
                    end: index + width,
                    content_start: quote + width,
                    content_end: index,
                    formatted,
                });
            } else {
                index += 1;
            }
        }
        Some(StringLiteral {
            end: self.characters.len(),
            content_start: quote + width,
            content_end: self.characters.len(),
            formatted,
        })
    }

    fn f_string_has_dynamic_code(&self, start: usize, end: usize) -> bool {
        let mut index = start;
        while let Some(expression_start) = self.next_f_expression(index, end) {
            let Some(expression_end) = self.f_expression_end(expression_start, end) else {
                return true;
            };
            if self.f_expression_is_dynamic(expression_start, expression_end) {
                return true;
            }
            index = expression_end + 1;
        }
        false
    }

    fn next_f_expression(&self, mut index: usize, end: usize) -> Option<usize> {
        while index < end {
            match (self.characters[index], self.characters.get(index + 1)) {
                ('{', Some('{')) => index += 2,
                ('{', _) => return Some(index + 1),
                _ => index += 1,
            }
        }
        None
    }

    fn f_expression_end(&self, start: usize, end: usize) -> Option<usize> {
        let mut depth = 1_u32;
        let mut index = start;
        while index < end {
            let token = self.expression_token(index);
            if token.brace_delta < 0 && depth == 1 {
                return Some(index);
            }
            depth = depth.saturating_add_signed(i32::from(token.brace_delta));
            index = token.next;
        }
        None
    }

    fn expression_token(&self, index: usize) -> ExpressionToken {
        if self.characters[index] == '#' {
            return ExpressionToken {
                next: self.comment_end(index),
                brace_delta: 0,
            };
        }
        if let Some(literal) = self.string_literal(index) {
            return ExpressionToken {
                next: literal.end,
                brace_delta: 0,
            };
        }
        let brace_delta = match self.characters[index] {
            '{' => 1,
            '}' => -1,
            _ => 0,
        };
        ExpressionToken { next: index + 1, brace_delta }
    }

    fn f_expression_is_dynamic(&self, start: usize, end: usize) -> bool {
        let expression: String = self.characters[start..end].iter().collect();
        Self::new(&expression).has_dynamic_code()
    }

    fn comment_end(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| *character != '\n') {
            index += 1;
        }
        index
    }

    fn skip_whitespace(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| character.is_whitespace()) {
            index += 1;
        }
        index
    }
}

#[derive(Clone, Copy)]
struct StringLiteral {
    end: usize,
    content_start: usize,
    content_end: usize,
    formatted: bool,
}

struct ExpressionToken {
    next: usize,
    brace_delta: i8,
}

fn dynamic_path(path: &[String]) -> bool {
    match path {
        [name] => matches!(name.as_str(), "builtins" | "compile" | "eval" | "exec" | "__builtins__"),
        [module, name] if matches!(module.as_str(), "builtins" | "__builtins__") => {
            matches!(name.as_str(), "compile" | "eval" | "exec")
        }
        [module, name] if module == "marshal" => name == "loads",
        [module, name] if module == "types" => matches!(name.as_str(), "CodeType" | "FunctionType"),
        _ => false,
    }
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::has_dynamic_code;

    #[test]
    fn executable_code_construction_is_rejected_without_matching_inert_text() {
        assert!(has_dynamic_code("exec(bytes.fromhex(payload))"));
        assert!(has_dynamic_code("runner = eval\nrunner(source)"));
        assert!(has_dynamic_code("builtins.compile(source, name, 'exec')"));
        assert!(has_dynamic_code("getattr(builtins, name)(source)"));
        assert!(has_dynamic_code("f'{exec(payload)}'"));
        assert!(has_dynamic_code("f'''{(\n# an inert } brace\nexec(payload)\n)}'''"));
        assert!(has_dynamic_code("types.FunctionType(marshal.loads(payload), globals())"));
        assert!(!has_dynamic_code("pattern = re.compile(r'exec\\(')"));
        assert!(!has_dynamic_code("f'exec is a built-in: {name}'"));
        assert!(!has_dynamic_code("f'{{exec}}: {name}'"));
        assert!(!has_dynamic_code("# exec(payload)\nprint('eval(source)')"));
        assert!(!has_dynamic_code("ast.literal_eval(source)"));
    }
}
