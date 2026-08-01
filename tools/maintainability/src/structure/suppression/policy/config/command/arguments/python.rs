mod evaluation;
mod process;

pub(super) fn join_implicit_continuations(source: &str) -> String {
    Scanner::new(source).scan()
}

#[cfg(test)]
pub(super) fn has_adjacent_string_literals(source: &str) -> bool {
    let normalized = join_implicit_continuations(source);
    has_adjacent_string_literals_in(&normalized)
}

pub(super) fn has_opaque_process_arguments(source: &str) -> bool {
    let normalized = join_implicit_continuations(source);
    if evaluation::has_dynamic_code(&normalized) {
        return true;
    }
    if references_command_capable_ffi(&normalized) {
        return true;
    }
    if imports_command_capable_api(&normalized) {
        return true;
    }
    if has_dynamic_process_resolution(&normalized) && references_rust_tool(&normalized) {
        return true;
    }
    if references_exec_or_spawn_api(&normalized) && references_rust_tool(&normalized) {
        return true;
    }
    if references_process_api(&normalized) && process::has_non_literal_arguments(&normalized) {
        return true;
    }
    normalized.lines().any(|line| {
        has_adjacent_string_literals_in(line) && (references_process_api(line) || references_rust_tool(line))
            || references_process_api(line) && AdjacentLiteralScanner::new(line).has_decoded_escape()
    })
}

fn imports_command_capable_api(source: &str) -> bool {
    source.lines().any(|line| {
        let compact = line.chars().filter(|character| !character.is_whitespace()).collect::<String>().to_ascii_lowercase();
        if compact.starts_with("importosas") || compact.starts_with("importsubprocessas") {
            return true;
        }
        let Some((module, imports)) = compact.strip_prefix("from").and_then(|line| line.split_once("import")) else {
            return false;
        };
        let imports = imports.trim_matches(['(', ')']);
        imports.split(',').any(|binding| {
            let binding = binding.trim_matches(['(', ')']);
            let name = binding.split_once("as").map_or(binding, |(name, _)| name);
            match module {
                "os" | "posix" => is_os_process_api(name),
                "subprocess" => is_subprocess_process_api(name),
                _ => false,
            }
        })
    })
}

fn is_os_process_api(name: &str) -> bool {
    matches!(name, "system" | "popen") || name.starts_with("exec") || name.starts_with("spawn")
}

fn is_subprocess_process_api(name: &str) -> bool {
    matches!(name, "call" | "check_call" | "check_output" | "getoutput" | "getstatusoutput" | "popen" | "run")
}

fn references_command_capable_ffi(source: &str) -> bool {
    let compact = AdjacentLiteralScanner::new(source)
        .without_literals()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "importctypes",
        "fromctypesimport",
        "importcffi",
        "fromcffiimport",
        "cdll(",
        "pydll(",
        "windll(",
        "oledll(",
        "cfunctype(",
        "pyfunctype(",
        "winfunctype(",
        ".dlopen(",
    ]
    .iter()
    .any(|name| compact.contains(name))
}

fn has_adjacent_string_literals_in(source: &str) -> bool {
    AdjacentLiteralScanner::new(source).has_adjacent_literals()
}

fn references_process_api(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    ["subprocess", "os.system", "os.popen", "posix.system", "posix.popen", "popen("]
        .iter()
        .any(|name| source.contains(name))
        || references_exec_or_spawn_api(&source)
}

fn references_exec_or_spawn_api(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    ["execl", "execv", "spawn"].iter().any(|name| source.contains(name))
}

fn has_dynamic_process_resolution(source: &str) -> bool {
    let compact = AdjacentLiteralScanner::new(source)
        .without_literals()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "__import__(",
        "getattr(",
        "globals(",
        "importlib.",
        "locals(",
        "operator.attrgetter(",
        "sys.modules",
        "vars(",
    ]
    .iter()
    .any(|name| compact.contains(name))
}

fn references_rust_tool(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    if ["cargo", "rustc", "rustdoc", "clippy-driver"].iter().any(|name| source.contains(name)) {
        return true;
    }
    let compact = source.chars().filter(|character| character.is_alphanumeric()).collect::<String>();
    ["cargo", "rustc", "rustdoc", "clippydriver"].iter().any(|name| compact.contains(name))
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

struct AdjacentLiteralScanner {
    characters: Vec<char>,
    index: usize,
}

impl AdjacentLiteralScanner {
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
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
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

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::{has_adjacent_string_literals, has_opaque_process_arguments};

    #[test]
    fn adjacent_literals_are_detected_only_within_one_python_expression() {
        assert!(has_adjacent_string_literals("subprocess.run([\"cargo\", \"clippy\", \"--\", \"-\" \"A\", \"warnings\"])\n"));
        assert!(has_adjacent_string_literals("VALUES = (r\"cargo\"  f\" clippy\")\n"));
        assert!(has_adjacent_string_literals("VALUE = \"cargo\" \\\n    \" clippy\"\n"));
        assert!(!has_adjacent_string_literals("\"module doc\"\n\"second statement\"\n"));
        assert!(!has_adjacent_string_literals("subprocess.run([\"cargo\", \"clippy\"])\n"));
        assert!(!has_adjacent_string_literals("identifier\"invalid but not concatenated\"\n"));
    }

    #[test]
    fn opaque_process_arguments_detect_executable_code_without_matching_inert_text() {
        assert!(has_opaque_process_arguments("subprocess.run([\"-\" \"A\"])\n"));
        assert!(has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", "\x2dA", "warnings"])"#));
        assert!(has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", b"\u002dA", "warnings"])"#));
        assert!(has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#));
        assert!(has_opaque_process_arguments(
            "import subprocess\narguments = ['cargo', 'clippy']\nsubprocess.run(arguments)\n"
        ));
        assert!(has_opaque_process_arguments(
            "from subprocess import run\narguments = ['cargo', 'clippy']\nrun(arguments)\n"
        ));
        assert!(has_opaque_process_arguments(r#"os.execlp("cargo", "cargo", "clippy", "--", "-" + "A", "warnings")"#));
        assert!(has_opaque_process_arguments(
            r#"from os import execvpe
execvpe("cargo", ["cargo", "clippy", "--", "-" + "A", "warnings"], environment)"#
        ));
        assert!(has_opaque_process_arguments(
            r#"from os import system as run
run(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"from os import system
system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"runner = __import__("sub" + "process")
runner.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#
        ));
        assert!(has_opaque_process_arguments(
            r#"runner = getattr(importlib.import_module("sub" + "process"), "r" + "un")
runner(["car" + "go", "clippy", "--", chr(45) + "A", "warnings"])"#
        ));
        assert!(has_opaque_process_arguments(
            r#"import ctypes
ctypes.CDLL(None).system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"from cffi import FFI
FFI().dlopen(None).system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"os.system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"posix.system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"posix.popen(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(
            r#"from posix import system as run
run(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
        ));
        assert!(has_opaque_process_arguments(r#"os.system("printf safe; " + command)"#));
        assert!(has_opaque_process_arguments(r#"subprocess.run(bytes.fromhex("2f7573722f62696e2f636172676f"))"#));
        assert!(has_opaque_process_arguments("subprocess.Popen(command)"));
        assert!(has_opaque_process_arguments(
            "import subprocess\nsubprocess.run([\"git\", \"status\"])\nrunner = subprocess.run\nrunner(bytes.fromhex(\"636172676f\").decode(), shell=True)\n"
        ));
        assert!(!has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", r"\x2dA", "warnings"])"#));
        assert!(!has_opaque_process_arguments(
            r#"subprocess.run(["cargo", "metadata", "--locked"], cwd=repository, check=True)"#
        ));
        assert!(!has_opaque_process_arguments(r#"subprocess.run(["git", "show", f"{reference}:{source}"], check=False)"#));
        assert!(!has_opaque_process_arguments(r#"subprocess.run([sys.executable, "script/check.py", value], check=True)"#));
        assert!(!has_opaque_process_arguments("from os import path\nprint(path.basename('/tmp/report'))"));
        assert!(!has_opaque_process_arguments("head = (f'<svg viewBox=\"0 0 64 64\" ' f'role=\"img\">')\n"));
        assert!(!has_opaque_process_arguments(
            "PATTERN = (r'^v[0-9]+' r'(?:-dev)?$')\nimport subprocess\nsubprocess.run(['git', 'status'])\n"
        ));
        assert!(!has_opaque_process_arguments("# import ctypes and run cargo\nprint('safe')\n"));
        assert!(!has_opaque_process_arguments(
            "\"\"\"getattr(importlib, 'run') and cargo are documentation only\"\"\"\nprint('safe')\n"
        ));
    }
}
