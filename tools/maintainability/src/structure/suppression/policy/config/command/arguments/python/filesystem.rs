use std::collections::BTreeSet;
use std::path::{Component, Path};

mod reviewed;

pub(super) fn is_reviewed_dynamic_write_surface(path: &str, source: &str) -> bool {
    reviewed::is_reviewed_dynamic_write_surface(path, source)
}

pub(super) fn has_opaque_write(source: &str) -> bool {
    if super::evaluation::has_non_ascii_code(source) {
        return true;
    }
    if has_opaque_writer_binding(source) {
        return true;
    }
    let mut scanner = CallScanner::new(source);
    let mut handled_methods = BTreeSet::new();
    while let Some(call) = scanner.next() {
        if handled_methods.contains(&call.start) {
            continue;
        }
        let chained_method = scanner.following_method(&call);
        if let Some(method) = &chained_method
            && is_path_mutation_method(&method.name)
        {
            handled_methods.insert(method.start);
        }
        if opaque_descriptor_write(call.name.as_str())
            || chained_method.as_ref().is_some_and(|method| chained_path_write_is_opaque(&scanner, &call, method))
            || direct_path_method_is_opaque(&scanner, &call)
            || direct_open_is_opaque(&scanner, &call)
            || descriptor_open_is_opaque(&scanner, &call)
            || mutation_arguments_are_opaque(&scanner, &call)
        {
            return true;
        }
    }
    scanner.opaque_formatted_write_expression
}

fn has_opaque_writer_binding(source: &str) -> bool {
    executable_statements(source).iter().any(|statement| writer_binding_is_opaque(statement))
}

fn executable_statements(source: &str) -> Vec<String> {
    CallScanner::new(source).executable_statements()
}

fn writer_binding_is_opaque(statement: &str) -> bool {
    let statement = statement_before_comment(statement).trim().trim_end_matches(';');
    let statement = strip_grouping_parentheses(statement);
    let Some((name, value)) = statement.split_once('=') else {
        return false;
    };
    let name = name.trim();
    let name = name.strip_suffix(':').unwrap_or(name).trim();
    let value = value.trim();
    if !is_identifier(name) || value.starts_with('=') {
        return false;
    }
    matches!(strip_grouping_parentheses(value), "open" | "builtins.open" | "io.open") || opaque_bound_path_method(value)
}

fn statement_before_comment(statement: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in statement.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote.is_some() {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_none() && character == '#' {
            return &statement[..index];
        }
    }
    statement
}

fn strip_grouping_parentheses(mut value: &str) -> &str {
    loop {
        value = value.trim();
        let Some(inner) = value.strip_prefix('(').and_then(|value| value.strip_suffix(')')) else {
            return value;
        };
        value = inner;
    }
}

fn opaque_bound_path_method(value: &str) -> bool {
    let mut scanner = CallScanner::new(value);
    let call = scanner.next();
    let Some(call) = call.filter(|call| is_path_constructor(&call.name)) else {
        return false;
    };
    let Some(method) = scanner.following_method_reference(&call) else {
        return false;
    };
    if !matches!(method.name.as_str(), "write_bytes" | "write_text") || !scanner.only_grouping_after(method.end) {
        return false;
    }
    path_argument_is_opaque(&scanner, call.opening_parenthesis, ArgumentSpec::new(0, &[]))
}

fn is_path_constructor(name: &str) -> bool {
    matches!(name, "Path" | "PosixPath" | "WindowsPath" | "pathlib.Path" | "pathlib.PosixPath" | "pathlib.WindowsPath")
}

fn opaque_descriptor_write(name: &str) -> bool {
    matches!(
        name,
        "os.copy_file_range" | "os.fchmod" | "os.fchown" | "os.ftruncate" | "os.pwrite" | "os.pwritev" | "os.sendfile" | "os.write" | "os.writev"
    )
}

fn mutation_arguments_are_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let arguments: &[ArgumentSpec] = match call.name.as_str() {
        "shutil.copy" | "shutil.copy2" | "shutil.copyfile" | "shutil.copytree" => &[ArgumentSpec::new(1, &["dst"])],
        "shutil.move" | "os.link" | "os.rename" | "os.renames" | "os.replace" => &[ArgumentSpec::new(0, &["src"]), ArgumentSpec::new(1, &["dst"])],
        "os.chmod" | "os.chown" | "os.lchown" | "os.makedirs" | "os.mkdir" | "os.remove" | "os.removedirs" | "os.rmdir" | "os.truncate" | "os.unlink" | "os.utime"
        | "shutil.rmtree" => &[ArgumentSpec::new(0, &["path", "name"])],
        "os.symlink" => return true,
        _ => return false,
    };
    arguments.iter().any(|argument| path_argument_is_opaque(scanner, call.opening_parenthesis, *argument))
}

fn chained_path_write_is_opaque(scanner: &CallScanner, call: &Call, method: &Call) -> bool {
    let receiver = ArgumentSpec::new(0, &[]);
    match method.name.as_str() {
        "chmod" | "lchmod" | "mkdir" | "rmdir" | "touch" | "unlink" | "write_bytes" | "write_text" => path_argument_is_opaque(scanner, call.opening_parenthesis, receiver),
        "open" => writable_mode(scanner, method.opening_parenthesis, 0) && path_argument_is_opaque(scanner, call.opening_parenthesis, receiver),
        "rename" | "replace" => {
            path_argument_is_opaque(scanner, call.opening_parenthesis, receiver) || path_argument_is_opaque(scanner, method.opening_parenthesis, ArgumentSpec::new(0, &["target"]))
        }
        "hardlink_to" | "symlink_to" => true,
        _ => false,
    }
}

fn direct_open_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    if !matches!(call.name.as_str(), "builtins.open" | "io.open" | "open") || !writable_mode(scanner, call.opening_parenthesis, 1) {
        return false;
    }
    path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["file"]))
}

fn direct_path_method_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let (owner, method) = call.name.rsplit_once('.').unwrap_or(("", call.name.as_str()));
    if matches!(owner, "builtins" | "io" | "os" | "shutil") || owner.is_empty() && method == "open" && !scanner.is_method_call(call.start) {
        return false;
    }
    let class_method = matches!(owner, "Path" | "PosixPath" | "WindowsPath" | "pathlib.Path" | "pathlib.PosixPath" | "pathlib.WindowsPath");
    if !class_method {
        return match method {
            "chmod" | "hardlink_to" | "lchmod" | "mkdir" | "rename" | "replace" | "rmdir" | "symlink_to" | "touch" | "unlink" | "write_bytes" | "write_text" => true,
            "open" => writable_mode(scanner, call.opening_parenthesis, 0),
            _ => false,
        };
    }
    let receiver = ArgumentSpec::new(0, &["self"]);
    match method {
        "chmod" | "lchmod" | "mkdir" | "rmdir" | "touch" | "unlink" | "write_bytes" | "write_text" => path_argument_is_opaque(scanner, call.opening_parenthesis, receiver),
        "open" => writable_mode(scanner, call.opening_parenthesis, 1) && path_argument_is_opaque(scanner, call.opening_parenthesis, receiver),
        "rename" | "replace" => {
            path_argument_is_opaque(scanner, call.opening_parenthesis, receiver) || path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(1, &["target"]))
        }
        "hardlink_to" | "symlink_to" => true,
        _ => false,
    }
}

fn is_path_mutation_method(method: &str) -> bool {
    matches!(
        method,
        "chmod" | "hardlink_to" | "lchmod" | "mkdir" | "open" | "rename" | "replace" | "rmdir" | "symlink_to" | "touch" | "unlink" | "write_bytes" | "write_text"
    )
}

fn descriptor_open_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    if call.name == "os.fdopen" {
        return writable_mode(scanner, call.opening_parenthesis, 1);
    }
    if call.name != "os.open" || !writable_os_flags(scanner, call.opening_parenthesis) {
        return false;
    }
    path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["path"]))
}

fn writable_os_flags(scanner: &CallScanner, opening_parenthesis: usize) -> bool {
    let Some(flags) = scanner.call_argument(opening_parenthesis, ArgumentSpec::new(1, &["flags"])) else {
        return true;
    };
    let flags = flags.chars().filter(|character| !character.is_whitespace()).collect::<String>();
    !matches!(flags.as_str(), "0" | "O_RDONLY" | "os.O_RDONLY")
}

fn writable_mode(scanner: &CallScanner, opening_parenthesis: usize, position: usize) -> bool {
    let Some(mode) = scanner.call_argument(opening_parenthesis, ArgumentSpec::new(position, &["mode"])) else {
        return false;
    };
    literal_value(&mode).is_none_or(|mode| mode.contains(['a', 'w', 'x', '+']))
}

fn path_argument_is_opaque(scanner: &CallScanner, opening_parenthesis: usize, argument: ArgumentSpec) -> bool {
    scanner
        .call_argument(opening_parenthesis, argument)
        .is_none_or(|argument| write_path_expression_is_opaque(&argument))
}

fn write_path_expression_is_opaque(argument: &str) -> bool {
    if let Some(path) = literal_value(argument) {
        return literal_write_path_is_opaque(&path);
    }
    true
}

fn literal_write_path_is_opaque(value: &str) -> bool {
    if value.contains(['$', '%', '{', '}', '*', '?']) {
        return true;
    }
    let value = value.replace('\\', "/");
    let path = Path::new(&value);
    path.is_absolute() || path.components().any(|component| !matches!(component, Component::CurDir | Component::Normal(_))) || is_execution_surface_path(&value)
}

#[derive(Clone, Copy)]
struct ArgumentSpec {
    position: usize,
    names: &'static [&'static str],
}

impl ArgumentSpec {
    const fn new(position: usize, names: &'static [&'static str]) -> Self {
        Self { position, names }
    }
}

fn is_execution_surface_path(value: &str) -> bool {
    if value.contains(['$', '%', '{', '}', '*', '?']) {
        return false;
    }
    let value = value.replace('\\', "/");
    let path = Path::new(&value);
    if path.is_absolute() || path.components().any(|component| !matches!(component, Component::CurDir | Component::Normal(_))) {
        return false;
    }
    let normalized = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    !normalized.is_empty() && super::super::super::is_execution_surface(&normalized)
}

struct Call {
    start: usize,
    name: String,
    opening_parenthesis: usize,
}

struct MethodReference {
    start: usize,
    name: String,
    end: usize,
}

struct CallScanner {
    characters: Vec<char>,
    index: usize,
    opaque_formatted_write_expression: bool,
}

impl CallScanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
            opaque_formatted_write_expression: false,
        }
    }

    fn executable_statements(&self) -> Vec<String> {
        let mut statements = Vec::new();
        let mut statement = String::new();
        let mut depth = 0_u32;
        let mut index = 0;
        while index < self.characters.len() {
            if let Some(literal) = self.string_literal(index) {
                statement.extend(&self.characters[index..literal.end]);
                index = literal.end;
                continue;
            }
            if self.characters[index] == '#' {
                index = self.skip_statement_comment(index, depth, &mut statements, &mut statement);
                continue;
            }
            let character = self.characters[index];
            match character {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                ';' | '\n' if depth == 0 => {
                    push_statement(&mut statements, &mut statement);
                    index += 1;
                    continue;
                }
                _ => {}
            }
            statement.push(character);
            index += 1;
        }
        push_statement(&mut statements, &mut statement);
        statements
    }

    fn skip_statement_comment(&self, index: usize, depth: u32, statements: &mut Vec<String>, statement: &mut String) -> usize {
        let mut next = self.comment_end(index);
        if depth == 0 {
            push_statement(statements, statement);
        } else {
            statement.push('\n');
        }
        if self.characters.get(next) == Some(&'\n') {
            next += 1;
        }
        next
    }

    fn next(&mut self) -> Option<Call> {
        while self.index < self.characters.len() {
            if self.characters[self.index] == '#' {
                self.skip_comment();
                continue;
            }
            if let Some(literal) = self.string_literal(self.index) {
                self.opaque_formatted_write_expression |= literal.formatted && self.formatted_string_has_opaque_write(&literal);
                self.index = literal.end;
                continue;
            }
            if !is_identifier_start(self.characters[self.index]) {
                self.index += 1;
                continue;
            }
            let start = self.index;
            self.index += 1;
            while self
                .characters
                .get(self.index)
                .is_some_and(|character| is_identifier_character(*character) || *character == '.')
            {
                self.index += 1;
            }
            let name = self.characters[start..self.index].iter().collect::<String>();
            let direct_opening = self.skip_whitespace(self.index);
            let opening_parenthesis = if self.characters.get(direct_opening) == Some(&'(') {
                Some(direct_opening)
            } else {
                self.grouped_identifier_invocation(start, self.index)
            };
            if let Some(opening_parenthesis) = opening_parenthesis {
                self.index = opening_parenthesis + 1;
                return Some(Call { start, name, opening_parenthesis });
            }
        }
        None
    }

    fn skip_comment(&mut self) {
        while self.characters.get(self.index).is_some_and(|character| *character != '\n') {
            self.index += 1;
        }
    }

    fn argument(&self, opening_parenthesis: usize, selected: usize) -> Option<String> {
        let mut index = opening_parenthesis + 1;
        let mut start = index;
        let mut argument = 0;
        let mut depth = 0_u32;
        while index < self.characters.len() {
            if self.characters[index] == '#' && depth == 0 {
                return None;
            }
            if let Some(end) = self.string_end(index) {
                index = end;
                continue;
            }
            match argument_boundary(self.characters[index], depth, argument, selected) {
                ArgumentBoundary::Selected => return Some(self.characters[start..index].iter().collect()),
                ArgumentBoundary::Next => {
                    argument += 1;
                    start = index + 1;
                    index += 1;
                    continue;
                }
                ArgumentBoundary::Missing => return None,
                ArgumentBoundary::None => {}
            }
            match self.characters[index] {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn call_argument(&self, opening_parenthesis: usize, selected: ArgumentSpec) -> Option<String> {
        let mut positional = 0;
        let mut index = 0;
        while let Some(argument) = self.argument(opening_parenthesis, index) {
            match keyword_argument(&argument) {
                Some((name, value)) if selected.names.contains(&name) => return Some(value.to_owned()),
                Some(_) => {}
                None if positional == selected.position => return Some(argument),
                None => positional += 1,
            }
            index += 1;
        }
        None
    }

    fn following_method(&self, call: &Call) -> Option<Call> {
        let method = self.following_method_reference(call)?;
        let direct_opening = self.skip_whitespace(method.end);
        let opening_parenthesis = if self.characters.get(direct_opening) == Some(&'(') {
            Some(direct_opening)
        } else {
            self.grouped_method_invocation(call.start, method.end)
        }?;
        Some(Call {
            start: method.start,
            name: method.name,
            opening_parenthesis,
        })
    }

    fn following_method_reference(&self, call: &Call) -> Option<MethodReference> {
        let mut index = self.closing_parenthesis(call.opening_parenthesis)? + 1;
        index = self.skip_whitespace(index);
        if self.characters.get(index) != Some(&'.') {
            return None;
        }
        index += 1;
        let start = index;
        while self.characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
            index += 1;
        }
        if index == start {
            return None;
        }
        let name = self.characters[start..index].iter().collect::<String>();
        Some(MethodReference { start, name, end: index })
    }

    fn grouped_identifier_invocation(&self, start: usize, end: usize) -> Option<usize> {
        let grouping = self.previous_non_whitespace(start)?;
        if self.characters[grouping] != '(' || !self.is_grouping_parenthesis(grouping) {
            return None;
        }
        let closing = self.skip_whitespace(end);
        if self.characters.get(closing) != Some(&')') || self.closing_parenthesis(grouping) != Some(closing) {
            return None;
        }
        let invocation = self.skip_whitespace(closing + 1);
        (self.characters.get(invocation) == Some(&'(')).then_some(invocation)
    }

    fn grouped_method_invocation(&self, receiver_start: usize, method_end: usize) -> Option<usize> {
        let grouping = self.previous_non_whitespace(receiver_start)?;
        if self.characters[grouping] != '(' || !self.is_grouping_parenthesis(grouping) {
            return None;
        }
        let closing = self.skip_whitespace(method_end);
        if self.characters.get(closing) != Some(&')') || self.closing_parenthesis(grouping) != Some(closing) {
            return None;
        }
        let invocation = self.skip_whitespace(closing + 1);
        (self.characters.get(invocation) == Some(&'(')).then_some(invocation)
    }

    fn previous_non_whitespace(&self, start: usize) -> Option<usize> {
        self.characters[..start].iter().rposition(|character| !character.is_whitespace())
    }

    fn is_grouping_parenthesis(&self, opening: usize) -> bool {
        !opening
            .checked_sub(1)
            .and_then(|index| self.characters.get(index))
            .is_some_and(|character| is_identifier_character(*character) || matches!(character, ')' | ']' | '}'))
    }

    fn only_grouping_after(&self, end: usize) -> bool {
        self.characters[end..].iter().all(|character| character.is_whitespace() || *character == ')')
    }

    fn closing_parenthesis(&self, opening_parenthesis: usize) -> Option<usize> {
        let mut index = opening_parenthesis + 1;
        let mut depth = 0_u32;
        while index < self.characters.len() {
            if let Some(end) = self.string_end(index) {
                index = end;
                continue;
            }
            match self.characters[index] {
                '(' | '[' | '{' => depth += 1,
                ')' if depth == 0 => return Some(index),
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn string_end(&self, start: usize) -> Option<usize> {
        self.string_literal(start).map(|literal| literal.end)
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
            } else if formatted && self.characters[index] == '{' {
                index = self.formatted_content_end(index);
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
        None
    }

    fn formatted_content_end(&self, opening: usize) -> usize {
        if self.characters.get(opening + 1) == Some(&'{') {
            return opening + 2;
        }
        self.f_expression_end(opening + 1, self.characters.len())
            .map_or(self.characters.len(), |closing| closing + 1)
    }

    fn formatted_string_has_opaque_write(&self, literal: &StringLiteral) -> bool {
        let content = self.characters[literal.content_start..literal.content_end].iter().collect::<String>();
        super::evaluation::formatted_code_expressions(&content).is_none_or(|expressions| expressions.iter().any(|expression| has_opaque_write(expression)))
    }

    fn f_expression_end(&self, start: usize, end: usize) -> Option<usize> {
        let mut depth = 1_u32;
        let mut index = start;
        while index < end {
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            if let Some(literal) = self.string_literal(index) {
                index = literal.end;
                continue;
            }
            match self.characters[index] {
                '{' => depth += 1,
                '}' if depth == 1 => return Some(index),
                '}' => depth -= 1,
                _ => {}
            }
            index += 1;
        }
        None
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

    fn is_method_call(&self, start: usize) -> bool {
        self.characters[..start].iter().rev().find(|character| !character.is_whitespace()) == Some(&'.')
    }
}

fn push_statement(statements: &mut Vec<String>, statement: &mut String) {
    let trimmed = statement.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_owned());
    }
    statement.clear();
}

struct StringLiteral {
    end: usize,
    content_start: usize,
    content_end: usize,
    formatted: bool,
}

fn keyword_argument(argument: &str) -> Option<(&str, &str)> {
    let (name, value) = argument.split_once('=')?;
    let name = name.trim();
    (!name.is_empty() && name.chars().all(is_identifier_character)).then_some((name, value.trim()))
}

enum ArgumentBoundary {
    None,
    Selected,
    Next,
    Missing,
}

const fn argument_boundary(character: char, depth: u32, argument: usize, selected: usize) -> ArgumentBoundary {
    match (character, depth, argument == selected) {
        (',' | ')', 0, true) => ArgumentBoundary::Selected,
        (',', 0, false) => ArgumentBoundary::Next,
        (')', 0, false) => ArgumentBoundary::Missing,
        _ => ArgumentBoundary::None,
    }
}

fn literal_value(argument: &str) -> Option<String> {
    let argument = argument.trim();
    let argument = argument.split_once('=').map_or(argument, |(name, value)| {
        if !name.is_empty() && name.chars().all(is_identifier_character) {
            value.trim()
        } else {
            argument
        }
    });
    let quote = argument.find(['\'', '"'])?;
    let prefix = &argument[..quote];
    if !prefix.chars().all(|character| matches!(character.to_ascii_lowercase(), 'b' | 'r' | 'u')) {
        return None;
    }
    let delimiter = argument.as_bytes()[quote] as char;
    let triple = argument[quote..].starts_with(&delimiter.to_string().repeat(3));
    let width = if triple { 3 } else { 1 };
    let closing = delimiter.to_string().repeat(width);
    let content = &argument[quote + width..];
    let content = content.strip_suffix(&closing)?;
    if prefix.to_ascii_lowercase().contains('r') {
        return Some(content.to_owned());
    }
    let mut value = String::with_capacity(content.len());
    let mut characters = content.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            let escaped = characters.next()?;
            if !matches!(escaped, '\\' | '/' | '\'' | '"') {
                return None;
            }
            value.push(escaped);
        } else {
            value.push(character);
        }
    }
    Some(value)
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters.next().is_some_and(is_identifier_start) && characters.all(is_identifier_character)
}

#[cfg(test)]
mod tests;
