use std::path::{Component, Path};

pub(super) fn mutates_literal_execution_surface(source: &str) -> bool {
    source.lines().any(|line| {
        let mut scanner = CallScanner::new(line);
        while let Some(call) = scanner.next() {
            let Some(destination) = destination_argument(call.name.as_str()) else {
                continue;
            };
            if scanner
                .argument(call.opening_parenthesis, destination)
                .is_some_and(|argument| literal_value(&argument).is_some_and(|path| is_execution_surface_path(&path)))
            {
                return true;
            }
        }
        false
    })
}

fn destination_argument(name: &str) -> Option<usize> {
    match name {
        "shutil.copy" | "shutil.copy2" | "shutil.copyfile" | "shutil.copytree" | "shutil.move" | "os.rename" | "os.renames" | "os.replace" => Some(1),
        _ => None,
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
                return None;
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
                return Some(Call { name, opening_parenthesis });
            }
        }
        None
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
mod tests {
    use super::mutates_literal_execution_surface;

    #[test]
    fn filesystem_copies_to_execution_surfaces_fail_closed() {
        for source in [
            r#"shutil.copyfile("quality/lint.data", "Justfile")"#,
            r#"shutil.copy2("quality/lint.data", "script/check.sh")"#,
            r#"shutil.copytree("quality/data", dst=".github/actions/check/action.yml")"#,
            r#"os.replace("quality/lint.data", r".cargo\config.toml")"#,
            r#"shutil.move("quality/lint.data", "script/check=lint.sh")"#,
        ] {
            assert!(mutates_literal_execution_surface(source), "{source}");
        }
    }

    #[test]
    fn filesystem_copies_to_data_paths_remain_allowed() {
        assert!(!mutates_literal_execution_surface(r#"shutil.copyfile("quality/report.txt", "target/report.txt")"#));
        assert!(!mutates_literal_execution_surface(r"shutil.copy2(source, destination)"));
        assert!(!mutates_literal_execution_surface(r#"print('shutil.copyfile("a", "Justfile")')"#));
    }
}
