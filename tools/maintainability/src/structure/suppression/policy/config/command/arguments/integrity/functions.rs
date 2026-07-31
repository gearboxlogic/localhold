use std::collections::BTreeSet;

use super::super::command_without_comment;
use super::{is_rust_tool_token, tokens, tool_basename};

pub(super) fn declares_required_command_override(source: &str, case_insensitive_tools: bool) -> bool {
    source
        .lines()
        .flat_map(|line| command_without_comment(line).split(';'))
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .filter_map(shell_function_name)
        .any(|name| is_required_command(name, case_insensitive_tools))
}

pub(super) fn quality_function_names(source: &str, case_insensitive_tools: bool) -> BTreeSet<String> {
    let definitions = function_definitions(source);
    let mut names = definitions
        .iter()
        .filter(|definition| super::contains_quality_command(definition.body, case_insensitive_tools))
        .map(|definition| definition.name.to_owned())
        .collect::<BTreeSet<_>>();
    loop {
        let discovered = definitions
            .iter()
            .filter(|definition| !names.contains(definition.name))
            .filter(|definition| body_calls_function(definition.body, &names))
            .map(|definition| definition.name.to_owned())
            .collect::<Vec<_>>();
        if discovered.is_empty() {
            return names;
        }
        names.extend(discovered);
    }
}

pub(super) fn has_unparsed_definition(source: &str) -> bool {
    let declared = source
        .lines()
        .flat_map(|line| command_without_comment(line).split(';'))
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .filter_map(shell_function_name)
        .count();
    declared != function_definitions(source).len()
}

pub(super) fn command_calls(command: &[String], names: &BTreeSet<String>) -> bool {
    let Some(index) = super::executable_index(command) else {
        return false;
    };
    let executable = &command[index];
    let arguments = &command[index + 1..];
    let executable_name = tool_basename(executable).to_ascii_lowercase();
    match super::super::references::wrapper::select(executable, &executable_name, arguments) {
        super::super::references::wrapper::Selection::NotWrapper => {}
        super::super::references::wrapper::Selection::NoCommand => return false,
        super::super::references::wrapper::Selection::Nested(nested) => return command_calls(nested, names),
        super::super::references::wrapper::Selection::Opaque => return command_calls(arguments, names),
    }
    let name = executable.trim_matches(['(', ')', '{', '}']);
    !name.contains(['/', '\\']) && names.contains(name)
}

pub(super) fn source_calls(source: &str, names: &BTreeSet<String>) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| command_calls(command, names))
}

fn body_calls_function(body: &str, names: &BTreeSet<String>) -> bool {
    source_calls(body, names)
}

fn is_required_command(name: &str, case_insensitive_tools: bool) -> bool {
    is_rust_tool_token(name, case_insensitive_tools) || tool_basename(name).eq_ignore_ascii_case("just")
}

struct FunctionDefinition<'a> {
    name: &'a str,
    body: &'a str,
}

fn function_definitions(source: &str) -> Vec<FunctionDefinition<'_>> {
    DefinitionScanner::new(source).scan()
}

struct DefinitionScanner<'a> {
    source: &'a str,
    index: usize,
    command_start: usize,
    quote: Option<char>,
    escaped: bool,
    comment: bool,
}

impl<'a> DefinitionScanner<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            index: 0,
            command_start: 0,
            quote: None,
            escaped: false,
            comment: false,
        }
    }

    fn scan(mut self) -> Vec<FunctionDefinition<'a>> {
        let mut definitions = Vec::new();
        while let Some(character) = self.current() {
            if self.advance_lexical_state(character) {
                continue;
            }
            if character == '{'
                && let Some((name, end)) = self.definition_at_current_brace()
            {
                definitions.push(FunctionDefinition {
                    name,
                    body: &self.source[self.index + 1..end],
                });
                self.index = end + 1;
                self.command_start = self.index;
                continue;
            }
            if matches!(character, '\n' | ';') {
                self.command_start = self.index + character.len_utf8();
            }
            self.index += character.len_utf8();
        }
        definitions
    }

    fn current(&self) -> Option<char> {
        self.source[self.index..].chars().next()
    }

    fn advance_lexical_state(&mut self, character: char) -> bool {
        if self.comment {
            self.comment = character != '\n';
            self.index += character.len_utf8();
            if !self.comment {
                self.command_start = self.index;
            }
            return true;
        }
        if self.escaped {
            self.escaped = false;
            self.index += character.len_utf8();
            return true;
        }
        if character == '\\' && self.quote != Some('\'') {
            self.escaped = true;
            self.index += 1;
            return true;
        }
        if matches!(character, '\'' | '"' | '`') {
            self.quote = updated_quote(self.quote, character);
            self.index += character.len_utf8();
            return true;
        }
        if self.quote.is_none() && character == '#' && starts_comment(self.source, self.index) {
            self.comment = true;
            self.index += 1;
            return true;
        }
        if self.quote.is_some() {
            self.index += character.len_utf8();
            return true;
        }
        false
    }

    fn definition_at_current_brace(&self) -> Option<(&'a str, usize)> {
        let current = &self.source[self.command_start..=self.index];
        let name = shell_function_name(current).or_else(|| {
            (current.trim() == "{")
                .then(|| previous_command(self.source, self.command_start))
                .flatten()
                .and_then(shell_function_name)
        })?;
        let end = matching_brace(self.source, self.index)?;
        Some((name, end))
    }
}

fn previous_command(source: &str, before: usize) -> Option<&str> {
    let prefix = source[..before].trim_end();
    let start = prefix.rfind(['\n', ';']).map_or(0, |index| index + 1);
    let command = prefix[start..].trim();
    (!command.is_empty()).then_some(command)
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 1_u32;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    for (offset, character) in source[open + 1..].char_indices() {
        let index = open + 1 + offset;
        if comment {
            comment = character != '\n';
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"' | '`') {
            quote = updated_quote(quote, character);
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match character {
            '#' if starts_comment(source, index) => comment = true,
            '{' => depth += 1,
            '}' if depth == 1 => return Some(index),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn starts_comment(source: &str, index: usize) -> bool {
    source[..index]
        .chars()
        .next_back()
        .is_none_or(|previous| previous.is_whitespace() || matches!(previous, ';' | '|' | '&' | '(' | ')' | '{' | '}'))
}

fn updated_quote(quote: Option<char>, character: char) -> Option<char> {
    if quote == Some(character) {
        None
    } else if quote.is_none() {
        Some(character)
    } else {
        quote
    }
}

fn shell_function_name(command: &str) -> Option<&str> {
    let command = command.trim_start();
    if let Some(rest) = command.strip_prefix("function").filter(|rest| rest.chars().next().is_some_and(char::is_whitespace)) {
        let rest = rest.trim_start();
        return shell_identifier(rest).map(|(name, _)| name);
    }
    let (name, rest) = shell_identifier(command)?;
    rest.trim_start().strip_prefix('(')?.trim_start().strip_prefix(')')?;
    Some(name)
}

fn shell_identifier(source: &str) -> Option<(&str, &str)> {
    let end = source
        .find(|character: char| !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.'))
        .unwrap_or(source.len());
    (end > 0 && !source.as_bytes()[0].is_ascii_digit()).then(|| source.split_at(end))
}

#[cfg(test)]
mod tests {
    use super::{declares_required_command_override, has_unparsed_definition, quality_function_names};

    #[test]
    fn required_command_functions_cannot_replace_executables() {
        assert!(declares_required_command_override("cargo() { :; }", true));
        assert!(declares_required_command_override("just() { :; }", true));
        assert!(declares_required_command_override("just() ( : )", true));
        let multiline = format!("cargo()\n{}\n    :\n{}", '{', '}');
        assert!(declares_required_command_override(&multiline, true));
        assert!(declares_required_command_override("function cargo { return }", true));
        assert!(!declares_required_command_override("printf '%s\n' 'cargo() {'", true));
        assert!(!declares_required_command_override("function report { return }", true));
    }

    #[test]
    fn non_brace_function_bodies_fail_closed() {
        assert!(has_unparsed_definition("gate()\n(\n just check-quality\n)\n"));
        assert!(!has_unparsed_definition("gate()\n{\n just check-quality\n}\n"));
    }

    #[test]
    fn quality_function_names_include_transitive_wrappers() {
        let source = "inner() {\n value=${name#prefix}\n just check-quality\n}\nouter()\n{\n inner\n true\n}\n";
        let names = quality_function_names(source, true);
        assert!(names.contains("inner"));
        assert!(names.contains("outer"));
        assert!(!quality_function_names("report() { printf '%s' cargo; }\n", true).contains("report"));
    }
}
