pub(super) fn has_non_literal_arguments(source: &str) -> bool {
    let mut scanner = ProcessCallScanner::new(source);
    let mut found_call = false;
    while let Some(opening_parenthesis) = scanner.next_call() {
        found_call = true;
        if !scanner.rust_process_argument_is_static(opening_parenthesis + 1) {
            return true;
        }
    }
    !found_call
}

struct ProcessCallScanner {
    characters: Vec<char>,
    index: usize,
}

impl ProcessCallScanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
        }
    }

    fn next_call(&mut self) -> Option<usize> {
        while self.index < self.characters.len() {
            if let Some(end) = self.string_literal_end(self.index) {
                self.index = end;
                continue;
            }
            if !is_identifier_start(self.characters[self.index]) {
                self.index += 1;
                continue;
            }
            let name_start = self.index;
            self.index += 1;
            while self
                .characters
                .get(self.index)
                .is_some_and(|character| is_identifier_character(*character) || *character == '.')
            {
                self.index += 1;
            }
            let name = self.characters[name_start..self.index].iter().collect::<String>();
            let opening_parenthesis = self.skip_whitespace(self.index);
            if self.characters.get(opening_parenthesis) == Some(&'(') && is_process_callable(&name) {
                self.index = opening_parenthesis + 1;
                return Some(opening_parenthesis);
            }
        }
        None
    }

    fn rust_process_argument_is_static(&self, start: usize) -> bool {
        let start = self.skip_whitespace(start);
        let Some(character) = self.characters.get(start) else {
            return false;
        };
        let first_literal = self.skip_whitespace(start + usize::from(matches!(character, '[' | '(')));
        let Some(first_literal_end) = self.string_literal_end(first_literal) else {
            return false;
        };
        if !self.literal_references_rust_tool(first_literal, first_literal_end) {
            return true;
        }
        let end = match character {
            '[' => self.static_string_sequence_end(start, ']'),
            '(' => self.static_string_sequence_end(start, ')'),
            _ => self.string_literal_end(start),
        };
        let Some(end) = end else {
            return false;
        };
        matches!(self.characters.get(self.skip_whitespace(end)), Some(',' | ')'))
    }

    fn literal_references_rust_tool(&self, start: usize, end: usize) -> bool {
        let literal = self.characters[start..end].iter().collect::<String>().to_ascii_lowercase();
        ["cargo", "rustc", "rustdoc", "clippy-driver"].iter().any(|tool| literal.contains(tool))
    }

    fn static_string_sequence_end(&self, start: usize, closing: char) -> Option<usize> {
        let mut index = self.skip_whitespace(start + 1);
        let mut found_literal = false;
        loop {
            if self.characters.get(index) == Some(&closing) {
                return found_literal.then_some(index + 1);
            }
            index = self.string_literal_end(index)?;
            found_literal = true;
            index = self.skip_whitespace(index);
            match self.characters.get(index) {
                Some(character) if *character == closing => return Some(index + 1),
                Some(',') => index = self.skip_whitespace(index + 1),
                _ => return None,
            }
        }
    }

    fn string_literal_end(&self, start: usize) -> Option<usize> {
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
            return None;
        }
        let mut quote = start;
        while quote < self.characters.len() && quote - start < 3 && matches!(self.characters[quote].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
            quote += 1;
        }
        if self.characters[start..quote].iter().any(|character| character.eq_ignore_ascii_case(&'f')) {
            return None;
        }
        let delimiter = *self.characters.get(quote).filter(|character| matches!(character, '\'' | '"'))?;
        let triple = self.characters.get(quote + 1) == Some(&delimiter) && self.characters.get(quote + 2) == Some(&delimiter);
        let width = if triple { 3 } else { 1 };
        let mut index = quote + width;
        while index < self.characters.len() {
            if self.characters[index] == '\\' {
                index = index.saturating_add(2);
            } else if self.characters[index] == delimiter && (!triple || self.characters.get(index + 1) == Some(&delimiter) && self.characters.get(index + 2) == Some(&delimiter)) {
                return Some(index + width);
            } else {
                index += 1;
            }
        }
        None
    }

    fn skip_whitespace(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| character.is_whitespace()) {
            index += 1;
        }
        index
    }
}

fn is_process_callable(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let basename = name.rsplit('.').next().unwrap_or(&name);
    name.starts_with("subprocess.") && matches!(basename, "run" | "call" | "check_call" | "check_output" | "getoutput" | "getstatusoutput" | "popen")
        || name.starts_with("os.") && (matches!(basename, "system" | "popen") || basename.starts_with("exec") || basename.starts_with("spawn"))
        || !name.contains('.') && (basename == "popen" || basename.starts_with("exec") || basename.starts_with("spawn"))
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::has_non_literal_arguments;

    #[test]
    fn rust_process_argv_must_be_an_inline_static_string_sequence() {
        assert!(!has_non_literal_arguments(
            r#"subprocess.run(["cargo", "metadata", "--locked"], cwd=repository, check=True)"#
        ));
        assert!(!has_non_literal_arguments(r#"os.system("cargo check")"#));
        assert!(!has_non_literal_arguments(r#"subprocess.run(["git", "show", f"{reference}:{source}"], check=False)"#));
        assert!(has_non_literal_arguments(r#"subprocess.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#));
        assert!(has_non_literal_arguments(r#"subprocess.run(["cargo"] + arguments)"#));
        assert!(has_non_literal_arguments(r#"subprocess.run([f"{tool}", "clippy"])"#));
        assert!(has_non_literal_arguments("subprocess.run(arguments)"));
        assert!(has_non_literal_arguments("from subprocess import run\nrun(arguments)"));
    }
}
