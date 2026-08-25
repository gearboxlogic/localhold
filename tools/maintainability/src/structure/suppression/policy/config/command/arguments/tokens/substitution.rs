mod lexical;
mod span;

pub(super) use lexical::without_active_continuations;
pub(super) use span::span_at;

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
    let mut commands = Vec::new();
    let complete = collect_commands(source, &mut commands, target, include_backticks);
    (commands, !complete)
}

fn collect_commands(source: &str, commands: &mut Vec<String>, target: SubstitutionKind, include_backticks: bool) -> bool {
    let mut index = 0;
    let mut lexical = lexical::State::default();
    while index < source.len() {
        let Some(character) = source[index..].chars().next() else {
            return false;
        };
        if lexical.can_open_substitution()
            && let Some(span) = nested_span_at(source, index, character, include_backticks, lexical.can_open_process_substitution())
        {
            let Ok(span) = span else {
                return false;
            };
            let command = &source[span.body_start..span.end];
            if !collect_commands(command, commands, target, include_backticks) {
                return false;
            }
            if records_substitution(target, character, command) {
                commands.push(command.to_owned());
            }
            index = span.end + 1;
            continue;
        }
        lexical.advance(source, index, character);
        index += character.len_utf8();
    }
    true
}

fn nested_span_at(source: &str, index: usize, character: char, include_backticks: bool, process_is_active: bool) -> Option<Result<span::Span, ()>> {
    if (include_backticks || character != '`')
        && let Some(span) = span::span_at(source, index)
    {
        return Some(span);
    }
    process_is_active.then(|| span::process_span_at(source, index)).flatten()
}

fn records_substitution(target: SubstitutionKind, opener: char, command: &str) -> bool {
    match target {
        SubstitutionKind::Process => matches!(opener, '<' | '>'),
        SubstitutionKind::Command => opener == '`' || opener == '$' && !command.starts_with('('),
    }
}

#[cfg(test)]
mod tests {
    use super::{command_commands, process_commands};

    #[test]
    fn executable_process_substitutions_are_extracted_without_inert_text() {
        assert_eq!(process_commands("cat <(sh quality/lint.txt)"), (vec!["sh quality/lint.txt".to_owned()], false));
        assert_eq!(
            process_commands("printf '%s' $'x\\''; cat <(sh quality/lint.txt)"),
            (vec!["sh quality/lint.txt".to_owned()], false)
        );
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
        assert_eq!(command_commands("printf \"$(ca\\\nse x in x) printf safe ;; esac)\"", true), (Vec::new(), true));
    }
}
