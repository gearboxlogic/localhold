pub(super) fn process_commands(source: &str) -> (Vec<String>, bool) {
    let characters = source.chars().collect::<Vec<_>>();
    let mut commands = Vec::new();
    let complete = collect_process_commands(&characters, &mut commands);
    (commands, !complete)
}

fn collect_process_commands(characters: &[char], commands: &mut Vec<String>) -> bool {
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut previous = None;
    while index < characters.len() {
        let character = characters[index];
        if comment {
            comment = character != '\n';
        } else if escaped {
            escaped = false;
        } else if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            }
        } else if character == '\\' {
            escaped = true;
        } else if quote == Some('"') && character == '"' {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_none() && character == '#' && comment_can_start_after(previous) {
            comment = true;
        } else if quote != Some('\'') && character == '`' {
            let Some(end) = backtick_end(characters, index + 1) else {
                return false;
            };
            if !collect_process_commands(&characters[index + 1..end], commands) {
                return false;
            }
            index = end;
        } else if quote != Some('\'') && character == '$' && characters.get(index + 1) == Some(&'(') {
            let Some(end) = parenthesized_end(characters, index + 2) else {
                return false;
            };
            if !collect_process_commands(&characters[index + 2..end], commands) {
                return false;
            }
            index = end;
        } else if quote.is_none() && matches!(character, '<' | '>') && characters.get(index + 1) == Some(&'(') {
            let Some(end) = parenthesized_end(characters, index + 2) else {
                return false;
            };
            let command = characters[index + 2..end].iter().collect::<String>();
            if !collect_process_commands(&characters[index + 2..end], commands) {
                return false;
            }
            commands.push(command);
            index = end;
        }
        previous = Some(character);
        index += 1;
    }
    true
}

fn parenthesized_end(characters: &[char], start: usize) -> Option<usize> {
    let mut depth = 1_usize;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut previous = None;
    for (index, character) in characters.iter().copied().enumerate().skip(start) {
        if comment {
            comment = character != '\n';
        } else if escaped {
            escaped = false;
        } else if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            }
        } else if character == '\\' {
            escaped = true;
        } else if quote == Some('"') && character == '"' {
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_none() && character == '#' && comment_can_start_after(previous) {
            comment = true;
        } else if quote.is_none() && character == '(' {
            depth += 1;
        } else if quote.is_none() && character == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        previous = Some(character);
    }
    None
}

fn comment_can_start_after(previous: Option<char>) -> bool {
    previous.is_none_or(|character| character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')'))
}

fn backtick_end(characters: &[char], start: usize) -> Option<usize> {
    let mut escaped = false;
    for (index, character) in characters.iter().copied().enumerate().skip(start) {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '`' {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::process_commands;

    #[test]
    fn executable_process_substitutions_are_extracted_without_inert_text() {
        assert_eq!(process_commands("cat <(sh quality/lint.txt)"), (vec!["sh quality/lint.txt".to_owned()], false));
        assert_eq!(process_commands("printf '%s' '<(sh quality/lint.txt)'"), (Vec::new(), false));
        assert_eq!(process_commands("printf ok # <(sh quality/lint.txt)"), (Vec::new(), false));
    }

    #[test]
    fn nested_and_malformed_substitutions_are_distinguished() {
        assert_eq!(
            process_commands("printf '%s' \"$(cat <(sh quality/lint.txt))\""),
            (vec!["sh quality/lint.txt".to_owned()], false)
        );
        assert_eq!(
            process_commands("diff <(sort <(cat left)) >(cat right)"),
            (vec!["cat left".to_owned(), "sort <(cat left)".to_owned(), "cat right".to_owned()], false,)
        );
        assert_eq!(
            process_commands("cat <(printf ok;# )\nsh quality/lint.txt\n)"),
            (vec!["printf ok;# )\nsh quality/lint.txt\n".to_owned()], false)
        );
        assert_eq!(process_commands("cat <(sh quality/lint.txt"), (Vec::new(), true));
    }
}
