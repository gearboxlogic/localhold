pub(super) fn process_commands(source: &str) -> (Vec<String>, bool) {
    substitution_commands(source, SubstitutionKind::Process, true)
}

pub(super) fn command_commands(source: &str, include_backticks: bool) -> (Vec<String>, bool) {
    substitution_commands(source, SubstitutionKind::Command, include_backticks)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubstitutionKind {
    Process,
    Command,
}

fn substitution_commands(source: &str, target: SubstitutionKind, include_backticks: bool) -> (Vec<String>, bool) {
    let characters = source.chars().collect::<Vec<_>>();
    let mut commands = Vec::new();
    let complete = collect_commands(&characters, &mut commands, target, include_backticks);
    (commands, !complete)
}

fn collect_commands(characters: &[char], commands: &mut Vec<String>, target: SubstitutionKind, include_backticks: bool) -> bool {
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
        } else if include_backticks && quote != Some('\'') && character == '`' {
            let Some(end) = backtick_end(characters, index + 1) else {
                return false;
            };
            let command = &characters[index + 1..end];
            if !collect_commands(command, commands, target, include_backticks) {
                return false;
            }
            if target == SubstitutionKind::Command {
                commands.push(command.iter().collect());
            }
            index = end;
        } else if quote != Some('\'') && character == '$' && characters.get(index + 1) == Some(&'(') {
            let Some(end) = parenthesized_end(characters, index + 2) else {
                return false;
            };
            let command = &characters[index + 2..end];
            if !collect_commands(command, commands, target, include_backticks) {
                return false;
            }
            if target == SubstitutionKind::Command && characters.get(index + 2) != Some(&'(') {
                commands.push(command.iter().collect());
            }
            index = end;
        } else if quote.is_none() && matches!(character, '<' | '>') && characters.get(index + 1) == Some(&'(') {
            let Some(end) = parenthesized_end(characters, index + 2) else {
                return false;
            };
            let command = &characters[index + 2..end];
            if !collect_commands(command, commands, target, include_backticks) {
                return false;
            }
            if target == SubstitutionKind::Process {
                commands.push(command.iter().collect());
            }
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
    use super::{command_commands, process_commands};

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

    #[test]
    fn command_substitutions_are_extracted_without_inert_or_arithmetic_text() {
        assert_eq!(
            command_commands(r#"printf '%s' "$(just check-quality)""#, true),
            (vec!["just check-quality".to_owned()], false)
        );
        assert_eq!(
            command_commands(r#"printf '%s' "$(printf '%s' "$(cargo clippy)")""#, true),
            (vec!["cargo clippy".to_owned(), r#"printf '%s' "$(cargo clippy)""#.to_owned(),], false,)
        );
        assert_eq!(command_commands("printf '%s' '$(just check-quality)'", true), (Vec::new(), false));
        assert_eq!(command_commands(r#"printf '%s' "$((1 + 2))""#, true), (Vec::new(), false));
        assert_eq!(
            command_commands(r#"printf '%s' "$(( $(cargo check) + 1 ))""#, true),
            (vec!["cargo check".to_owned()], false)
        );
        assert_eq!(command_commands("printf `%s`", true), (vec!["%s".to_owned()], false));
        assert_eq!(command_commands("Write-Output \"build``stamp\"", false), (Vec::new(), false));
        assert_eq!(command_commands("printf \"$(just check-quality\"", true), (Vec::new(), true));
    }
}
