mod bindings;
mod evaluation;
mod execution;
mod filesystem;
mod process;

pub(super) use execution::References as ExecutionReferences;

pub(super) fn execution_references(path: &str, source: &str) -> ExecutionReferences {
    let normalized = normalize_continuations(source);
    let mut references = execution::collect(&normalized);
    references.opaque |= has_opaque_process_bindings(&normalized) && !bindings::is_reviewed_surface(path, source);
    references
}

pub(super) fn is_reviewed_dynamic_surface(path: &str, source: &str) -> bool {
    filesystem::is_reviewed_dynamic_write_surface(path, source)
}

pub(super) fn is_reviewed_process_surface(path: &str, source: &str) -> bool {
    process::is_reviewed_surface(path, source)
}

pub(super) fn normalize_continuations(source: &str) -> String {
    Scanner::new(source).scan()
}

#[cfg(test)]
pub(super) fn has_adjacent_string_literals(source: &str) -> bool {
    let normalized = normalize_continuations(source);
    has_adjacent_string_literals_in(&normalized)
}

pub(super) fn has_opaque_process_arguments(path: &str, source: &str) -> bool {
    let normalized = normalize_continuations(source);
    let reviewed_process_binding_surface = bindings::is_reviewed_surface(path, source);
    let reviewed_dynamic_surface = evaluation::is_reviewed_dynamic_code_surface(path, source);
    let reviewed_process_surface = process::is_reviewed_surface(path, source);
    if evaluation::has_dynamic_code(&normalized) && !reviewed_dynamic_surface {
        return true;
    }
    if references_command_capable_ffi(&normalized) {
        return true;
    }
    if has_opaque_process_bindings(&normalized) && !reviewed_process_binding_surface {
        return true;
    }
    if has_direct_dynamic_process_resolution(&normalized) && !reviewed_dynamic_surface {
        return true;
    }
    if has_dynamic_process_resolution(&normalized) && references_rust_tool(&normalized) {
        return true;
    }
    if references_exec_or_spawn_api(&normalized) && references_rust_tool(&normalized) {
        return true;
    }
    if references_process_api(&normalized) && process::has_non_literal_arguments(&normalized) && !reviewed_process_surface {
        return true;
    }
    normalized.lines().any(|line| {
        has_adjacent_string_literals_in(line) && (references_process_api(line) || references_rust_tool(line))
            || references_process_api(line) && AdjacentLiteralScanner::new(line).has_decoded_escape()
    })
}

pub(super) fn has_opaque_process_bindings(source: &str) -> bool {
    imports_command_capable_api(source) || uses_command_module_as_value(source) || uses_command_callable_as_value(source) || uses_dynamic_namespace_callable_as_value(source)
}

pub(super) fn mutates_process_working_directory(source: &str) -> bool {
    let compact = executable_code(source).chars().filter(|character| !character.is_whitespace()).collect::<String>();
    ["os.chdir(", "os.fchdir(", "posix.chdir(", "posix.fchdir(", "contextlib.chdir("]
        .iter()
        .any(|call| compact.contains(call))
}

pub(super) fn mutates_process_environment(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    let compact = executable.chars().filter(|character| !character.is_whitespace()).collect::<String>();
    if [
        "os.putenv(",
        "os.unsetenv(",
        "posix.putenv(",
        "posix.unsetenv(",
        "os.environ.update(",
        "os.environ.clear(",
        "os.environ.pop(",
        "os.environ.popitem(",
        "os.environ.setdefault(",
        "os.environ.__setitem__(",
        "os.environ.__delitem__(",
    ]
    .iter()
    .any(|operation| compact.contains(operation))
    {
        return true;
    }
    executable.lines().any(environment_reference_is_opaque)
}

fn executable_code(source: &str) -> String {
    AdjacentLiteralScanner::new(source).without_literals().to_ascii_lowercase()
}

fn normalized_qualified_code(source: &str) -> String {
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

fn environment_reference_is_opaque(line: &str) -> bool {
    let mut remainder = line;
    while let Some((head, tail)) = remainder.split_once("os.environ") {
        let tail = tail.trim_start();
        if let Some(indexed) = tail.strip_prefix('[') {
            let Some(after_subscript) = after_matching_subscript(indexed) else {
                return true;
            };
            if has_delete_keyword(head) || starts_assignment(after_subscript.trim_start()) {
                return true;
            }
        } else if ![".get(", ".copy(", ".keys(", ".items(", ".values("].iter().any(|prefix| tail.starts_with(prefix)) {
            return true;
        }
        remainder = tail;
    }
    false
}

fn has_delete_keyword(head: &str) -> bool {
    let statement = head.rsplit(';').next().unwrap_or(head);
    statement.match_indices("del").any(|(index, keyword)| {
        let before = statement[..index].chars().next_back();
        let after = statement[index + keyword.len()..].chars().next();
        !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character)
    })
}

fn after_matching_subscript(source: &str) -> Option<&str> {
    let mut expected = vec![']'];
    for (index, character) in source.char_indices() {
        if let Some(closing) = closing_delimiter(character) {
            expected.push(closing);
        } else if matches!(character, ')' | ']' | '}') {
            if expected.pop() != Some(character) {
                return None;
            }
            if expected.is_empty() {
                return Some(&source[index + character.len_utf8()..]);
            }
        }
    }
    None
}

fn starts_assignment(source: &str) -> bool {
    source.starts_with('=') && !source.starts_with("==")
        || ["+=", "-=", "*=", "/=", "//=", "%=", "**=", "&=", "|=", "^=", ">>=", "<<="]
            .iter()
            .any(|operator| source.starts_with(operator))
        || source.strip_prefix(':').is_some_and(annotation_has_assignment)
}

fn annotation_has_assignment(annotation: &str) -> bool {
    let mut expected = Vec::new();
    let characters = annotation.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().copied().enumerate() {
        if let Some(closing) = closing_delimiter(character) {
            expected.push(closing);
        } else if matches!(character, ')' | ']' | '}') {
            if expected.pop() != Some(character) {
                return true;
            }
        } else if character == '=' && expected.is_empty() {
            let before = index.checked_sub(1).and_then(|previous| characters.get(previous)).copied();
            let after = characters.get(index + 1).copied();
            if after != Some('=') && !matches!(before, Some('=' | '!' | '<' | '>' | ':')) {
                return true;
            }
        }
    }
    !expected.is_empty()
}

const fn closing_delimiter(character: char) -> Option<char> {
    match character {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

pub(super) fn has_opaque_filesystem_write(path: &str, source: &str) -> bool {
    filesystem::has_opaque_write(&normalize_continuations(source)) && !filesystem::is_reviewed_dynamic_write_surface(path, source)
}

fn imports_command_capable_api(source: &str) -> bool {
    executable_code(source).lines().flat_map(|line| line.split(';')).any(|statement| {
        let compact = statement.chars().filter(|character| !character.is_whitespace()).collect::<String>();
        if compact.strip_prefix("import").is_some_and(|imports| imports.split(',').any(command_module_alias)) {
            return true;
        }
        let Some((module, imports)) = compact.strip_prefix("from").and_then(|line| line.split_once("import")) else {
            return false;
        };
        let imports = imports.trim_matches(['(', ')']);
        imports.split(',').any(|binding| {
            let binding = binding.trim_matches(['(', ')']);
            let name = binding.split_once("as").map_or(binding, |(name, _)| name);
            if is_command_module(name) {
                return true;
            }
            match module {
                "asyncio" => name == "*" || matches!(name, "create_subprocess_exec" | "create_subprocess_shell"),
                "os" | "posix" => name == "*" || is_os_process_api(name),
                "subprocess" => name == "*" || is_subprocess_process_api(name),
                "pty" => matches!(name, "*" | "spawn"),
                "contextlib" => matches!(name, "*" | "chdir"),
                "sys" => matches!(name, "*" | "modules"),
                _ => false,
            }
        })
    })
}

fn command_module_alias(binding: &str) -> bool {
    ["asyncio", "contextlib", "os", "posix", "pty", "subprocess", "sys"]
        .iter()
        .any(|module| binding.strip_prefix(module).is_some_and(|suffix| suffix.starts_with("as") && suffix.len() > 2))
}

fn uses_command_module_as_value(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    executable
        .lines()
        .flat_map(|line| line.split(';'))
        .filter(|statement| !starts_python_keyword(statement, "import") && !starts_python_keyword(statement, "from"))
        .any(|statement| {
            ["asyncio", "contextlib", "os", "posix", "pty", "subprocess", "sys"]
                .iter()
                .any(|module| standalone_module_value(statement, module) || nested_module_value(statement, module))
        })
}

fn uses_command_callable_as_value(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    executable
        .lines()
        .flat_map(|line| line.split(';'))
        .any(|statement| qualified_command_references(statement).any(|reference| reference.is_opaque()))
}

fn uses_dynamic_namespace_callable_as_value(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    executable.lines().flat_map(|line| line.split(';')).any(|statement| {
        ["getattr", "globals", "locals", "vars"].iter().any(|name| callable_name_is_value(statement, name)) || callable_name_is_value(statement, "operator.attrgetter")
    })
}

fn callable_name_is_value(statement: &str, name: &str) -> bool {
    statement.match_indices(name).any(|(index, _)| {
        let before = statement[..index].chars().next_back();
        let remainder = &statement[index + name.len()..];
        let after = remainder.chars().next();
        !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character) && !remainder.trim_start().starts_with('(')
    })
}

struct QualifiedCommandReference<'a> {
    remainder: &'a str,
    kind: CommandReferenceKind,
}

impl QualifiedCommandReference<'_> {
    fn is_opaque(&self) -> bool {
        let remainder = self.remainder.trim_start();
        match self.kind {
            CommandReferenceKind::Callable => !remainder.starts_with('('),
            CommandReferenceKind::Environment => !remainder.starts_with('['),
        }
    }
}

#[derive(Clone, Copy)]
enum CommandReferenceKind {
    Callable,
    Environment,
}

fn qualified_command_references(statement: &str) -> impl Iterator<Item = QualifiedCommandReference<'_>> {
    statement.match_indices('.').filter_map(|(dot, _)| {
        let module_start = statement[..dot]
            .char_indices()
            .rev()
            .take_while(|(_, character)| is_identifier_character(*character))
            .last()
            .map_or(dot, |(index, _)| index);
        let module = &statement[module_start..dot];
        if module_start > 0 && statement[..module_start].ends_with('.') {
            return None;
        }
        let attribute_end = statement[dot + 1..]
            .char_indices()
            .take_while(|(_, character)| is_identifier_character(*character) || *character == '.')
            .last()
            .map_or(dot + 1, |(index, character)| dot + 1 + index + character.len_utf8());
        let attribute = &statement[dot + 1..attribute_end];
        let (kind, consumed) = command_attribute_prefix(module, attribute)?;
        Some(QualifiedCommandReference {
            remainder: &statement[dot + 1 + consumed..],
            kind,
        })
    })
}

fn command_attribute_prefix(module: &str, attribute: &str) -> Option<(CommandReferenceKind, usize)> {
    let first = attribute.split('.').next().unwrap_or_default();
    match module {
        "os" | "posix" => {
            if first == "__getattribute__" {
                return Some((CommandReferenceKind::Callable, first.len()));
            }
            if matches!(first, "chdir" | "fchdir" | "popen" | "putenv" | "startfile" | "system" | "unsetenv") || first.starts_with("exec") || first.starts_with("spawn") {
                return Some((CommandReferenceKind::Callable, first.len()));
            }
            if let Some(method) = attribute.strip_prefix("environ.").and_then(|suffix| suffix.split('.').next()) {
                if matches!(method, "update" | "clear" | "pop" | "popitem" | "setdefault" | "__setitem__" | "__delitem__") {
                    return Some((CommandReferenceKind::Callable, "environ.".len() + method.len()));
                }
                if matches!(method, "get" | "copy" | "keys" | "items" | "values") {
                    return None;
                }
            }
            (first == "environ").then_some((CommandReferenceKind::Environment, first.len()))
        }
        "subprocess" if is_subprocess_process_api(first) || first == "__getattribute__" => Some((CommandReferenceKind::Callable, first.len())),
        "asyncio" if matches!(first, "create_subprocess_exec" | "create_subprocess_shell" | "__getattribute__") => Some((CommandReferenceKind::Callable, first.len())),
        "pty" if matches!(first, "spawn" | "__getattribute__") => Some((CommandReferenceKind::Callable, first.len())),
        "contextlib" if matches!(first, "chdir" | "__getattribute__") => Some((CommandReferenceKind::Callable, first.len())),
        "sys" if first == "__getattribute__" => Some((CommandReferenceKind::Callable, first.len())),
        _ => None,
    }
}

fn starts_python_keyword(statement: &str, keyword: &str) -> bool {
    statement.trim_start().strip_prefix(keyword).is_some_and(|tail| tail.starts_with(char::is_whitespace))
}

fn standalone_module_value(statement: &str, module: &str) -> bool {
    statement.match_indices(module).any(|(index, _)| {
        let before = statement[..index].chars().next_back();
        let after = statement[index + module.len()..].chars().next();
        if before == Some('.') || before.is_some_and(is_identifier_character) || after.is_some_and(is_identifier_character) {
            return false;
        }
        !statement[index + module.len()..].trim_start().starts_with('.')
    })
}

fn nested_module_value(statement: &str, module: &str) -> bool {
    let reference = format!(".{module}");
    statement.match_indices(&reference).any(|(index, _)| {
        let end = index + reference.len();
        statement[..index].chars().next_back().is_some_and(is_identifier_character) && !statement[end..].chars().next().is_some_and(is_identifier_character)
    })
}

fn is_command_module(name: &str) -> bool {
    matches!(name, "asyncio" | "contextlib" | "os" | "posix" | "pty" | "subprocess" | "sys")
}

fn is_os_process_api(name: &str) -> bool {
    matches!(
        name,
        "chdir" | "environ" | "fchdir" | "popen" | "posix_spawn" | "posix_spawnp" | "putenv" | "startfile" | "system" | "unsetenv"
    ) || name.starts_with("exec")
        || name.starts_with("spawn")
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
    [
        "asyncio.create_subprocess_",
        "os.system",
        "os.popen",
        "os.startfile",
        "posix.system",
        "posix.popen",
        "pty.spawn",
    ]
    .iter()
    .any(|name| source.contains(name))
        || references_root_module(&source, "subprocess")
        || references_exec_or_spawn_api(&source)
}

fn references_root_module(source: &str, module: &str) -> bool {
    source.match_indices(module).any(|(index, _)| {
        let before = source[..index].chars().next_back();
        let after = source[index + module.len()..].chars().next();
        before != Some('.') && !before.is_some_and(is_identifier_character) && after == Some('.')
    })
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

fn has_direct_dynamic_process_resolution(source: &str) -> bool {
    let compact = AdjacentLiteralScanner::new(source)
        .without_literals()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact.contains("sys.modules")
        || ["getattr(", "globals(", "locals(", "operator.attrgetter(", "vars()"]
            .iter()
            .any(|access| compact.contains(access))
    {
        return true;
    }
    ["asyncio", "contextlib", "os", "posix", "pty", "subprocess", "sys"].iter().any(|module| {
        compact.contains(&format!("{module}.__dict__"))
            || compact.contains(&format!("{module}.__getattribute__("))
            || compact.contains(&format!("getattr({module},"))
            || compact.contains(&format!("vars({module})"))
    })
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
mod tests;
