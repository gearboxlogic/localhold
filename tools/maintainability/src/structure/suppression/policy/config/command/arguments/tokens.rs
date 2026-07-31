mod substitution;

pub(super) fn process_substitution_commands(source: &str) -> (Vec<String>, bool) {
    substitution::process_commands(source)
}

pub(super) fn normalized_shell_tokens(source: &str) -> Vec<String> {
    normalized_shell_commands(source).into_iter().flatten().collect()
}

pub(super) fn normalized_shell_commands(source: &str) -> Vec<Vec<String>> {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    shell_command_segments(&logical)
        .into_iter()
        .map(|segment| command_tokens(command_without_comment(segment)))
        .collect()
}

pub(super) fn source_command_tokens(source: &str) -> Vec<Vec<String>> {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    shell_command_segments(&logical)
        .into_iter()
        .map(|segment| shell_tokens(command_without_comment(segment), false))
        .collect()
}

fn shell_command_segments(source: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut previous: Option<char> = None;
    for (index, character) in source.char_indices() {
        if comment {
            if character == '\n' {
                segments.push(&source[start..index]);
                start = index + character.len_utf8();
                comment = false;
            }
        } else if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        } else if quote.is_none() && character == '#' && previous.is_none_or(|value| value.is_whitespace() || matches!(value, ';' | '&' | '|')) {
            comment = true;
        } else if quote.is_none() && matches!(character, '\n' | ';' | '&' | '|') {
            segments.push(&source[start..index]);
            start = index + character.len_utf8();
        }
        previous = Some(character);
    }
    segments.push(&source[start..]);
    segments
}

pub(super) fn without_noncommand_shell_data(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut pending = std::collections::VecDeque::<Heredoc>::new();
    let mut array_assignment = false;
    let mut case_pattern_expected = Vec::new();
    for line in source.lines() {
        if let Some(heredoc) = pending.front() {
            let candidate = if heredoc.strip_tabs { line.trim_start_matches('\t') } else { line };
            if candidate == heredoc.delimiter {
                pending.pop_front();
            }
            output.push('\n');
            continue;
        }
        if array_assignment {
            if line.contains("$(") || line.contains('`') {
                output.push_str(line);
            }
            output.push('\n');
            array_assignment = !array_assignment_closes(line);
            continue;
        }
        if let Some(contents) = shell_array_assignment_contents(line) {
            if line.contains("$(") || line.contains('`') {
                output.push_str(line);
            }
            output.push('\n');
            array_assignment = !array_assignment_closes(contents);
            continue;
        }
        let mut command = line;
        let trimmed = line.trim_start();
        if case_pattern_expected.last() == Some(&true) {
            if trimmed.starts_with("esac") {
                case_pattern_expected.pop();
            } else if let Some(index) = case_pattern_end(trimmed) {
                command = &trimmed[index..];
                *case_pattern_expected.last_mut().expect("a case pattern is active") = false;
            }
        } else if trimmed.starts_with("esac") {
            case_pattern_expected.pop();
        }
        output.push_str(command);
        output.push('\n');
        pending.extend(heredocs_on_line(line));
        if starts_case_statement(command) {
            case_pattern_expected.push(true);
        }
        if has_case_arm_terminator(command)
            && let Some(expected) = case_pattern_expected.last_mut()
        {
            *expected = true;
        }
    }
    output
}

pub(super) fn has_executable_unquoted_heredoc(source: &str) -> bool {
    let mut pending = std::collections::VecDeque::<Heredoc>::new();
    for line in source.lines() {
        if let Some(heredoc) = pending.front() {
            let candidate = if heredoc.strip_tabs { line.trim_start_matches('\t') } else { line };
            if candidate == heredoc.delimiter {
                pending.pop_front();
            } else if heredoc.allows_expansion && active_heredoc_substitution(line) {
                return true;
            }
            continue;
        }
        pending.extend(heredocs_on_line(line));
    }
    false
}

fn active_heredoc_substitution(source: &str) -> bool {
    let mut escaped = false;
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '`' || character == '$' && characters.peek() == Some(&'(') {
            return true;
        }
    }
    false
}

fn starts_case_statement(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("case ") && line.ends_with(" in")
}

fn case_pattern_end(line: &str) -> Option<usize> {
    unquoted_character_index(line, ')').map(|index| index + 1)
}

fn has_case_arm_terminator(line: &str) -> bool {
    [";;", ";&", ";;&"].iter().any(|terminator| line.contains(terminator))
}

fn unquoted_character_index(source: &str, needle: char) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        } else if quote.is_none() && character == needle {
            return Some(index);
        }
    }
    None
}

fn shell_array_assignment_contents(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let name_end = line.find(|character: char| !character.is_ascii_alphanumeric() && character != '_')?;
    let name = &line[..name_end];
    if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    line[name_end..].strip_prefix("=(")
}

fn array_assignment_closes(line: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        } else if character == ')' && quote.is_none() {
            return true;
        }
    }
    false
}

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    allows_expansion: bool,
}

fn heredocs_on_line(line: &str) -> Vec<Heredoc> {
    let characters = line.chars().collect::<Vec<_>>();
    let mut heredocs = Vec::new();
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    while index + 1 < characters.len() {
        let character = characters[index];
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        } else if quote.is_none() && character == '<' && characters[index + 1] == '<' && characters.get(index + 2) != Some(&'<') {
            index += 2;
            let strip_tabs = characters.get(index) == Some(&'-');
            index += usize::from(strip_tabs);
            while characters.get(index).is_some_and(|character| character.is_whitespace()) {
                index += 1;
            }
            if let Some((delimiter, end, allows_expansion)) = heredoc_delimiter(&characters, index) {
                heredocs.push(Heredoc {
                    delimiter,
                    strip_tabs,
                    allows_expansion,
                });
                index = end;
                continue;
            }
        }
        index += 1;
    }
    heredocs
}

fn heredoc_delimiter(characters: &[char], start: usize) -> Option<(String, usize, bool)> {
    let mut index = start;
    let mut quote = None;
    let mut delimiter = String::new();
    let mut escaped = false;
    let mut quoted = false;
    while let Some(character) = characters.get(index).copied() {
        if escaped {
            delimiter.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
            quoted = true;
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
                quoted = true;
            } else if quote.is_none() {
                quote = Some(character);
                quoted = true;
            } else {
                delimiter.push(character);
            }
        } else if quote.is_none() && (character.is_whitespace() || matches!(character, ';' | '|' | '&' | '<' | '>' | '(' | ')')) {
            break;
        } else {
            delimiter.push(character);
        }
        index += 1;
    }
    (quote.is_none() && !escaped && !delimiter.is_empty()).then_some((delimiter, index, !quoted))
}

pub(super) fn command_without_comment(segment: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    let mut previous = None;
    for (index, character) in segment.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
        } else if character == '#' && quote.is_none() && previous.is_none_or(char::is_whitespace) {
            return &segment[..index];
        }
        previous = Some(character);
    }
    segment
}

pub(super) fn command_tokens(command: &str) -> Vec<String> {
    shell_tokens(command, true)
}

fn shell_tokens(command: &str, split_equals: bool) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            token.push(character);
            escaped = false;
        } else if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            } else {
                token.push(character);
            }
        } else if quote == Some('"') {
            if character == '"' {
                quote = None;
            } else if character == '\\' {
                escaped = true;
            } else {
                token.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            if character == '\'' && token.ends_with('$') {
                token.push(character);
            }
            quote = Some(character);
        } else if character == '\\' {
            escaped = true;
        } else if character.is_whitespace() || split_equals && character == '=' {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if escaped {
        token.push('\\');
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::{command_tokens, has_executable_unquoted_heredoc, source_command_tokens, without_noncommand_shell_data};

    #[test]
    fn ansi_c_quoted_words_retain_an_execution_marker() {
        assert_eq!(command_tokens("$'\\x73\\x68' quality/lint.txt"), vec!["$'\\x73\\x68", "quality/lint.txt"]);
        assert_eq!(command_tokens("printf '%s' \"$'\\x73'\""), vec!["printf", "%s", "$'x73'"]);
    }

    #[test]
    fn quoted_program_text_does_not_create_command_fragments() {
        let commands = source_command_tokens("digest=$(printf '%s' value | sed 's/../\\\\x&/g')\n/tmp/lint");
        assert!(!commands.iter().flatten().any(|token| token == "/g)"));
        assert!(commands.iter().any(|tokens| tokens.first().is_some_and(|token| token == "/tmp/lint")));
    }

    #[test]
    fn separators_in_comments_are_not_commands() {
        let commands = source_command_tokens("printf ok # /tmp/ignored & /tmp/also-ignored\nprintf done");
        assert_eq!(commands.len(), 2);
        assert!(!commands.iter().flatten().any(|token| token.starts_with("/tmp/")));
    }

    #[test]
    fn heredoc_payloads_are_not_parsed_as_shell_commands() {
        let source = "cat <<'DOC'\n  PREFIX/bin/hold\nDOC\ncat <<-SCRIPT\n\t./generated-command\n\tSCRIPT\nquality/run-lints\n";
        let normalized = without_noncommand_shell_data(source);
        assert!(!normalized.contains("PREFIX/bin/hold"));
        assert!(!normalized.contains("./generated-command"));
        assert!(normalized.contains("quality/run-lints"));
    }

    #[test]
    fn executable_unquoted_heredoc_substitutions_fail_closed() {
        assert!(has_executable_unquoted_heredoc("cat <<DOC\n$(sh quality/lint.txt)\nDOC\n"));
        assert!(has_executable_unquoted_heredoc("cat <<DOC\n'$(sh quality/lint.txt)'\nDOC\n"));
        assert!(has_executable_unquoted_heredoc("cat <<-DOC\n\t`sh quality/lint.txt`\n\tDOC\n"));
        assert!(!has_executable_unquoted_heredoc("cat <<'DOC'\n$(sh quality/lint.txt)\nDOC\n"));
        assert!(!has_executable_unquoted_heredoc("cat <<D\"OC\"\n$(sh quality/lint.txt)\nDOC\n"));
        assert!(!has_executable_unquoted_heredoc("cat <<DOC\n\\$(sh quality/lint.txt)\nDOC\n"));
    }

    #[test]
    fn shell_array_literals_are_not_parsed_as_commands() {
        let source = "markers=(\n  'C:\\\\Users\\\\'\n  'docs/review/*'\n)\npaths=(. ':(exclude)script/check.sh')\nquality/run-lints\n";
        let normalized = without_noncommand_shell_data(source);
        assert!(!normalized.contains("Users"));
        assert!(!normalized.contains("exclude"));
        assert!(normalized.contains("quality/run-lints"));
    }

    #[test]
    fn case_patterns_are_not_parsed_as_commands() {
        let source = "case \"$path\" in\n  docs/*|script/retired-command.sh)\n    quality/run-lints\n    ;;\n  inline) quality/check-format ;;\nesac\n";
        let normalized = without_noncommand_shell_data(source);
        assert!(!normalized.contains("script/retired-command.sh"));
        assert!(normalized.contains("quality/run-lints"));
        assert!(normalized.contains("quality/check-format"));
    }
}
