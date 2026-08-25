use super::{ArgumentSpec, CallScanner, is_identifier_character};

impl CallScanner {
    fn argument(&self, opening_parenthesis: usize, selected: usize) -> Option<String> {
        let mut index = opening_parenthesis + 1;
        let mut argument = 0;
        let mut depth = 0_u32;
        let mut value = String::new();
        while index < self.characters.len() {
            if let Some(end) = self.string_end(index) {
                append_selected_slice(&mut value, &self.characters[index..end], argument == selected);
                index = end;
                continue;
            }
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                append_selected_character(&mut value, '\n', argument == selected);
                continue;
            }
            match argument_boundary(self.characters[index], depth, argument, selected) {
                ArgumentBoundary::Selected => return Some(value),
                ArgumentBoundary::Next => {
                    argument += 1;
                    index += 1;
                    continue;
                }
                ArgumentBoundary::Missing => return None,
                ArgumentBoundary::None => {}
            }
            if argument == selected {
                value.push(self.characters[index]);
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

    pub(super) fn call_is_malformed(&self, opening_parenthesis: usize) -> bool {
        self.closing_parenthesis(opening_parenthesis).is_none()
    }

    pub(super) fn call_argument(&self, opening_parenthesis: usize, selected: ArgumentSpec) -> Option<String> {
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

    pub(super) fn has_argument_unpack(&self, opening_parenthesis: usize) -> bool {
        let mut index = opening_parenthesis + 1;
        let mut depth = 0_u32;
        let mut argument_start = true;
        while index < self.characters.len() {
            if let Some(end) = self.string_end(index) {
                argument_start = false;
                index = end;
                continue;
            }
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            match self.characters[index] {
                '*' if depth == 0 && argument_start => return true,
                '(' | '[' | '{' => {
                    depth += 1;
                    argument_start = false;
                }
                ')' if depth == 0 => return false,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => argument_start = true,
                character if character.is_whitespace() && argument_start => {}
                _ if depth == 0 => argument_start = false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    pub(super) fn closing_parenthesis(&self, opening_parenthesis: usize) -> Option<usize> {
        let mut index = opening_parenthesis + 1;
        let mut expected = vec![')'];
        while index < self.characters.len() {
            if let Some(end) = self.string_end(index) {
                index = end;
                continue;
            }
            if self.starts_string_literal(index) {
                return None;
            }
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            match self.characters[index] {
                '(' => expected.push(')'),
                '[' => expected.push(']'),
                '{' => expected.push('}'),
                closing @ (')' | ']' | '}') => match expected.pop() {
                    Some(expected_closing) if expected_closing == closing && expected.is_empty() => return Some(index),
                    Some(expected_closing) if expected_closing == closing => {}
                    _ => return None,
                },
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn starts_string_literal(&self, start: usize) -> bool {
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
            return false;
        }
        let mut quote = start;
        while quote < self.characters.len() && quote - start < 3 && matches!(self.characters[quote].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
            quote += 1;
        }
        self.characters.get(quote).is_some_and(|character| matches!(character, '\'' | '"'))
    }
}

fn keyword_argument(argument: &str) -> Option<(&str, &str)> {
    let (name, value) = argument.split_once('=')?;
    let name = name.trim();
    (!name.is_empty() && name.chars().all(is_identifier_character)).then_some((name, value.trim()))
}

fn append_selected_slice(value: &mut String, characters: &[char], selected: bool) {
    if selected {
        value.extend(characters.iter().copied());
    }
}

fn append_selected_character(value: &mut String, character: char, selected: bool) {
    if selected {
        value.push(character);
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
