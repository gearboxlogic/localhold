pub(super) fn has_non_literal_arguments(source: &str) -> bool {
    let mut found_call = false;
    for line in source.lines() {
        let mut scanner = ProcessCallScanner::new(line);
        while let Some((opening_parenthesis, kind)) = scanner.next_call() {
            found_call = true;
            if !scanner.process_argument_is_static(opening_parenthesis + 1, kind) {
                return true;
            }
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

    fn next_call(&mut self) -> Option<(usize, ProcessKind)> {
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
            if let Some(kind) = self
                .characters
                .get(opening_parenthesis)
                .filter(|character| **character == '(')
                .and_then(|_| process_kind(&name))
            {
                self.index = opening_parenthesis + 1;
                return Some((opening_parenthesis, kind));
            }
        }
        None
    }

    fn process_argument_is_static(&self, start: usize, kind: ProcessKind) -> bool {
        let start = self.skip_whitespace(start);
        let Some(character) = self.characters.get(start) else {
            return false;
        };
        let first_literal = self.skip_whitespace(start + usize::from(matches!(character, '[' | '(')));
        if kind == ProcessKind::Argv
            && let Some(end) = self.sys_executable_end(first_literal)
        {
            return self.first_argv_element_ends_at(end, *character);
        }
        let Some(first_literal_end) = self.string_literal_end(first_literal) else {
            return false;
        };
        if kind == ProcessKind::Shell {
            return self.argument_ends_at(first_literal_end);
        }
        if !self.literal_references_rust_tool(first_literal, first_literal_end) {
            return self.first_argv_element_ends_at(first_literal_end, *character);
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

    fn argument_ends_at(&self, end: usize) -> bool {
        matches!(self.characters.get(self.skip_whitespace(end)), Some(',' | ')'))
    }

    fn first_argv_element_ends_at(&self, end: usize, container: char) -> bool {
        let next = self.characters.get(self.skip_whitespace(end));
        match container {
            '[' => matches!(next, Some(',' | ']')),
            '(' => matches!(next, Some(',' | ')')),
            _ => matches!(next, Some(',' | ')')),
        }
    }

    fn sys_executable_end(&self, start: usize) -> Option<usize> {
        const SYS_EXECUTABLE: &[char] = &['s', 'y', 's', '.', 'e', 'x', 'e', 'c', 'u', 't', 'a', 'b', 'l', 'e'];
        (self.characters.get(start..start + SYS_EXECUTABLE.len()) == Some(SYS_EXECUTABLE)
            && self
                .characters
                .get(start + SYS_EXECUTABLE.len())
                .is_none_or(|character| !is_identifier_character(*character) && *character != '.'))
        .then_some(start + SYS_EXECUTABLE.len())
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProcessKind {
    Argv,
    Shell,
}

fn process_kind(name: &str) -> Option<ProcessKind> {
    let name = name.to_ascii_lowercase();
    let basename = name.rsplit('.').next().unwrap_or(&name);
    if name.starts_with("subprocess.") && matches!(basename, "getoutput" | "getstatusoutput")
        || name.starts_with("os.") && matches!(basename, "system" | "popen")
        || !name.contains('.') && basename == "popen"
    {
        return Some(ProcessKind::Shell);
    }
    if name.starts_with("subprocess.") && matches!(basename, "run" | "call" | "check_call" | "check_output" | "popen")
        || name.starts_with("os.") && (basename.starts_with("exec") || basename.starts_with("spawn"))
        || !name.contains('.') && (basename.starts_with("exec") || basename.starts_with("spawn"))
    {
        return Some(ProcessKind::Argv);
    }
    None
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
    fn process_executables_and_shell_commands_must_be_static() {
        assert!(!has_non_literal_arguments(
            r#"subprocess.run(["cargo", "metadata", "--locked"], cwd=repository, check=True)"#
        ));
        assert!(!has_non_literal_arguments(r#"os.system("cargo check")"#));
        assert!(!has_non_literal_arguments(r#"subprocess.run(["git", "show", f"{reference}:{source}"], check=False)"#));
        assert!(!has_non_literal_arguments(r#"subprocess.run([sys.executable, "script/check.py", value])"#));
        assert!(!has_non_literal_arguments(
            "def inspect():\n    \"\"\"Inspect one file.\"\"\"\n    subprocess.run([\"readelf\", \"-d\", str(path)], check=False)\n"
        ));
        assert!(has_non_literal_arguments(r#"subprocess.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#));
        assert!(has_non_literal_arguments(r#"subprocess.run(["cargo"] + arguments)"#));
        assert!(has_non_literal_arguments(r#"subprocess.run([f"{tool}", "clippy"])"#));
        assert!(has_non_literal_arguments(r#"subprocess.run(["car" + "go", "clippy"])"#));
        assert!(has_non_literal_arguments(r#"subprocess.run([sys.executable.replace("python", "cargo"), "clippy"])"#));
        assert!(has_non_literal_arguments("subprocess.run(arguments)"));
        assert!(has_non_literal_arguments("from subprocess import run\nrun(arguments)"));
        assert!(has_non_literal_arguments(r#"os.system(bytes.fromhex("636172676f"))"#));
        assert!(has_non_literal_arguments(r#"os.system("printf safe; " + command)"#));
        assert!(has_non_literal_arguments(r#"subprocess.run(bytes.fromhex("2f7573722f62696e2f636172676f"))"#));
    }
}
