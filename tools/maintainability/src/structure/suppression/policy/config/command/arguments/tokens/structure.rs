pub(super) fn commands(source: &str) -> Vec<super::StructuredCommand> {
    let commands_only = super::without_noncommand_shell_data(source);
    let logical = commands_only.replace("\\\r\n", "").replace("\\\n", "");
    super::segments::shell_command_segments(&logical)
        .into_iter()
        .map(|segment| {
            let source = super::command_without_comment(segment.source);
            let groups = structural_groups(source);
            super::StructuredCommand {
                source: source.to_owned(),
                words: super::shell_tokens(source, false),
                open_braces: groups.open_braces,
                close_braces: groups.close_braces,
                open_subshells: groups.open_subshells,
                close_subshells: groups.close_subshells,
                conditionally_executed: segment.conditionally_executed,
                isolated: segment.isolated,
            }
        })
        .collect()
}

#[derive(Default)]
struct StructuralGroups {
    open_braces: usize,
    close_braces: usize,
    open_subshells: usize,
    close_subshells: usize,
}

fn structural_groups(source: &str) -> StructuralGroups {
    let mut groups = StructuralGroups::default();
    let mut quote = None;
    let mut escaped = false;
    let mut substitution_depth = 0_usize;
    let mut previous = None;
    for (index, character) in source.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            quote = updated_quote(quote, character);
        } else if substitution_opener(source, index, substitution_depth, character, previous) {
            substitution_depth += 1;
        } else if quote.is_none() && substitution_depth > 0 && character == ')' {
            substitution_depth -= 1;
        } else if quote.is_none() && substitution_depth == 0 && structural_group(source, index, previous) {
            groups.open_braces += usize::from(character == '{');
            groups.close_braces += usize::from(character == '}');
            groups.open_subshells += usize::from(character == '(');
            groups.close_subshells += usize::from(character == ')');
        }
        previous = Some(character);
    }
    groups
}

fn updated_quote(quote: Option<char>, character: char) -> Option<char> {
    if quote == Some(character) { None } else { quote.or(Some(character)) }
}

fn substitution_opener(source: &str, index: usize, depth: usize, character: char, previous: Option<char>) -> bool {
    source[index..].starts_with("$(")
        || source[index..].starts_with("<(")
        || source[index..].starts_with(">(")
        || depth > 0 && character == '(' && !matches!(previous, Some('$' | '<' | '>'))
}

fn structural_group(source: &str, index: usize, previous: Option<char>) -> bool {
    let character = source[index..].chars().next().expect("character index");
    let next = source[index + character.len_utf8()..].chars().next();
    matches!(character, '{' | '}' | '(' | ')')
        && !matches!(
            (character, previous, next),
            ('(', Some('('), _) | ('(', _, Some('(')) | (')', Some(')'), _) | (')', _, Some(')'))
        )
        && previous.is_none_or(group_boundary)
        && next.is_none_or(group_boundary)
}

const fn group_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')' | '<' | '>')
}

#[cfg(test)]
mod tests {
    use super::commands;

    #[test]
    fn grammar_continuation_newlines_and_comments_preserve_execution_context() {
        for source in ["false &&\nlocal value\n", "false && # explanation\nlocal value\n", "true |\nlocal value\n"] {
            let parsed = commands(source);
            let local = parsed.iter().find(|command| command.words.iter().any(|word| word == "local")).expect("local command");
            assert!(local.conditionally_executed || local.isolated, "{source}");
        }

        let parsed = commands("true\nlocal value\n");
        let local = parsed.iter().find(|command| command.words.iter().any(|word| word == "local")).expect("local command");
        assert!(!local.conditionally_executed);
        assert!(!local.isolated);
    }

    #[test]
    fn backgrounding_marks_the_complete_and_or_or_brace_list_as_isolated() {
        for source in ["first && second &\n", "{ first; second; } &\n"] {
            let commands = commands(source);
            for command in commands
                .iter()
                .filter(|command| command.words.iter().any(|word| matches!(word.as_str(), "first" | "second")))
            {
                assert!(command.isolated, "{source}: {:?}", command.words);
            }
        }

        let commands = commands("first\nsecond &\n");
        let first = commands.iter().find(|command| command.words.iter().any(|word| word == "first")).expect("first command");
        assert!(!first.isolated);
    }

    #[test]
    fn quoted_conditional_closers_do_not_enable_internal_operators() {
        for source in [
            "[[ value == ')) || payload' ]] && local value\n",
            "[[ $(printf '%s' ')) || payload') == value ]] && local value\n",
            "printf '%s' \"$((1))\" && local value\n",
        ] {
            let parsed = commands(source);
            let local = parsed.iter().find(|command| command.words.iter().any(|word| word == "local")).expect("local command");
            assert!(local.conditionally_executed, "{source}");
        }

        let parsed = commands("printf '%s' \"$(printf '%s' ')) || inert')\"\nlocal value\n");
        assert_eq!(parsed.iter().filter(|command| command.words.iter().any(|word| word == "local")).count(), 1);
    }
}
