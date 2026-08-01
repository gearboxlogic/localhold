use std::path::{Component, Path};

pub(super) fn has_opaque_write(source: &str) -> bool {
    let mut scanner = CallScanner::new(source);
    while let Some(call) = scanner.next() {
        if opaque_descriptor_write(call.name.as_str())
            || chained_path_write_is_opaque(&scanner, &call)
            || direct_path_method_is_opaque(&scanner, &call)
            || direct_open_is_opaque(&scanner, &call)
            || descriptor_open_is_opaque(&scanner, &call)
            || mutation_arguments_are_opaque(&scanner, &call)
        {
            return true;
        }
    }
    false
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
    arguments
        .iter()
        .any(|argument| literal_argument_is_execution_surface(scanner, call.opening_parenthesis, *argument))
}

fn chained_path_write_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let Some(method) = scanner.following_method(call.opening_parenthesis) else {
        return false;
    };
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
    let Some((owner, method)) = call.name.rsplit_once('.') else {
        return false;
    };
    if !matches!(owner, "Path" | "PosixPath" | "WindowsPath" | "pathlib.Path" | "pathlib.PosixPath" | "pathlib.WindowsPath") {
        return false;
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
    !joined_literal_filename_is_safe(argument)
}

fn joined_literal_filename_is_safe(argument: &str) -> bool {
    let argument = argument.trim();
    let mut scanner = CallScanner::new(argument);
    let Some(call) = scanner.next().filter(|call| call.name == "os.path.join") else {
        return false;
    };
    if scanner.characters[..call.start].iter().any(|character| !character.is_whitespace()) {
        return false;
    }
    let Some(closing) = scanner.closing_parenthesis(call.opening_parenthesis) else {
        return false;
    };
    if scanner.characters[closing + 1..].iter().any(|character| !character.is_whitespace()) {
        return false;
    }
    let mut index = 0;
    let mut final_argument = None;
    while let Some(argument) = scanner.argument(call.opening_parenthesis, index) {
        final_argument = Some(argument);
        index += 1;
    }
    let Some(filename) = final_argument.as_deref().and_then(literal_value) else {
        return false;
    };
    let filename = filename.replace('\\', "/");
    let path = Path::new(&filename);
    !filename.is_empty() && path.file_name().and_then(|name| name.to_str()) == Some(filename.as_str()) && !literal_write_path_is_opaque(&filename)
}

fn literal_write_path_is_opaque(value: &str) -> bool {
    if value.contains(['$', '%', '{', '}', '*', '?']) {
        return true;
    }
    let value = value.replace('\\', "/");
    let path = Path::new(&value);
    path.is_absolute() || path.components().any(|component| !matches!(component, Component::CurDir | Component::Normal(_))) || is_execution_surface_path(&value)
}

fn literal_argument_is_execution_surface(scanner: &CallScanner, opening_parenthesis: usize, argument: ArgumentSpec) -> bool {
    scanner
        .call_argument(opening_parenthesis, argument)
        .is_some_and(|argument| literal_value(&argument).is_some_and(|path| is_execution_surface_path(&path)))
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

struct CallScanner {
    characters: Vec<char>,
    index: usize,
}

impl CallScanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
        }
    }

    fn next(&mut self) -> Option<Call> {
        while self.index < self.characters.len() {
            if self.characters[self.index] == '#' {
                self.skip_comment();
                continue;
            }
            if let Some(end) = self.string_end(self.index) {
                self.index = end;
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
            let opening_parenthesis = self.skip_whitespace(self.index);
            if self.characters.get(opening_parenthesis) == Some(&'(') {
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

    fn following_method(&self, opening_parenthesis: usize) -> Option<Call> {
        let mut index = self.closing_parenthesis(opening_parenthesis)? + 1;
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
        let opening_parenthesis = self.skip_whitespace(index);
        (self.characters.get(opening_parenthesis) == Some(&'(')).then_some(Call { start, name, opening_parenthesis })
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
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
            return None;
        }
        let mut quote = start;
        while quote < self.characters.len() && quote - start < 3 && matches!(self.characters[quote].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
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

#[cfg(test)]
mod tests;
