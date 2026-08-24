mod heredoc;
mod structure;
mod substitution;

pub(super) struct StructuredCommand {
    pub(super) words: Vec<String>,
    pub(super) open_braces: usize,
    pub(super) close_braces: usize,
}

pub(super) fn declared_shell_functions(source: &str) -> std::collections::BTreeSet<String> {
    shell_function_declarations(source).into_iter().collect()
}

pub(super) fn declared_shell_function_count(source: &str, expected: &str) -> usize {
    shell_function_declarations(source).iter().filter(|name| name.as_str() == expected).count()
}

fn shell_function_declarations(source: &str) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut pending = None;
    for line in source.lines() {
        let line = line.trim();
        if let Some(name) = pending.take() {
            if line.is_empty() || line.starts_with('#') {
                pending = Some(name);
                continue;
            }
            if line.starts_with('{') {
                declarations.push(name);
                continue;
            }
        }
        let (declaration, has_brace) = line.find('{').map_or((line, false), |brace| (&line[..brace], true));
        let Some(name) = shell_function_name(declaration.trim_end()) else {
            continue;
        };
        if has_brace {
            declarations.push(name.to_owned());
        } else {
            pending = Some(name.to_owned());
        }
    }
    declarations
}

fn shell_function_name(declaration: &str) -> Option<&str> {
    let name = if let Some(declaration) = declaration.strip_prefix("function ") {
        declaration.trim().trim_end_matches("()").trim_end()
    } else {
        declaration.strip_suffix("()")?.trim_end()
    };
    (!name.is_empty() && !name.as_bytes()[0].is_ascii_digit() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')).then_some(name)
}

pub(super) fn process_substitution_commands(source: &str) -> (Vec<String>, bool) {
    substitution::process_commands(source)
}

pub(super) fn command_substitution_commands(source: &str, include_backticks: bool) -> (Vec<String>, bool) {
    substitution::command_commands(source, include_backticks)
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

pub(super) fn structured_source_commands(source: &str) -> Vec<StructuredCommand> {
    structure::commands(source)
}

fn shell_command_segments(source: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut conditional = false;
    let mut substitution_depth = 0_usize;
    let mut previous: Option<char> = None;
    for (index, character) in source.char_indices() {
        if comment {
            if character == '\n' && substitution_depth == 0 {
                segments.push(&source[start..index]);
                start = index + character.len_utf8();
            }
            if character == '\n' {
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
        } else if quote.is_none()
            && (source[index..].starts_with("$(")
                || source[index..].starts_with("<(")
                || source[index..].starts_with(">(")
                || substitution_depth > 0 && character == '(' && !matches!(previous, Some('$' | '<' | '>')))
        {
            substitution_depth += 1;
        } else if quote.is_none() && substitution_depth > 0 && character == ')' {
            substitution_depth -= 1;
        } else if quote.is_none()
            && substitution_depth == 0
            && !conditional
            && (shell_conditional_opener(source, index, previous, "[[") || shell_conditional_opener(source, index, previous, "(("))
        {
            conditional = true;
        } else if quote.is_none() && substitution_depth == 0 && conditional && (shell_conditional_closer(source, index, "]]") || shell_conditional_closer(source, index, "))")) {
            conditional = false;
        } else if quote.is_none()
            && substitution_depth == 0
            && !conditional
            && (matches!(character, '\n' | ';' | '|') || character == '&' && !matches!(previous, Some('<' | '>')) && !source[index + character.len_utf8()..].starts_with('>'))
        {
            segments.push(&source[start..index]);
            start = index + character.len_utf8();
        }
        previous = Some(character);
    }
    segments.push(&source[start..]);
    segments
}

fn shell_conditional_opener(source: &str, index: usize, previous: Option<char>, delimiter: &str) -> bool {
    source[index..].starts_with(delimiter) && previous.is_none_or(shell_word_boundary) && source[index + delimiter.len()..].chars().next().is_none_or(shell_word_boundary)
}

fn shell_conditional_closer(source: &str, index: usize, delimiter: &str) -> bool {
    source[index..].starts_with(delimiter) && source[index + delimiter.len()..].chars().next().is_none_or(shell_word_boundary)
}

const fn shell_word_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')')
}

pub(super) fn without_noncommand_shell_data(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut pending = std::collections::VecDeque::<heredoc::Document>::new();
    let mut heredoc_scan = heredoc::Scan::default();
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
            } else if let Some(command) = after_array_assignment(line) {
                output.push_str(command);
            }
            output.push('\n');
            array_assignment = !array_assignment_closes(line);
            continue;
        }
        if let Some(contents) = shell_array_assignment_contents(line) {
            if line.contains("$(") || line.contains('`') {
                output.push_str(line);
            } else if let Some(command) = after_array_assignment(contents) {
                output.push_str(command);
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
            } else {
                command = "";
            }
        } else if trimmed.starts_with("esac") {
            case_pattern_expected.pop();
        }
        output.push_str(command);
        output.push('\n');
        pending.extend(heredoc_scan.documents(line));
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

pub(super) fn shell_expansion_source(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut pending = std::collections::VecDeque::<heredoc::Document>::new();
    let mut heredoc_scan = heredoc::Scan::default();
    for line in source.lines() {
        if let Some(heredoc) = pending.front() {
            let candidate = if heredoc.strip_tabs { line.trim_start_matches('\t') } else { line };
            if candidate == heredoc.delimiter {
                pending.pop_front();
            } else if heredoc.allows_expansion {
                output.extend(line.chars().map(|character| match character {
                    '\'' | '"' | '#' => ' ',
                    _ => character,
                }));
            }
            output.push('\n');
            continue;
        }
        output.push_str(line);
        output.push('\n');
        pending.extend(heredoc_scan.documents(line));
    }
    output
}

pub(super) fn has_executable_unquoted_heredoc(source: &str) -> bool {
    let mut pending = std::collections::VecDeque::<heredoc::Document>::new();
    let mut heredoc_scan = heredoc::Scan::default();
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
        pending.extend(heredoc_scan.documents(line));
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

fn after_array_assignment(line: &str) -> Option<&str> {
    let closing = unquoted_character_index(line, ')')?;
    Some(&line[closing + ')'.len_utf8()..])
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

pub(super) fn update_substitution_state(word: &str, depth: &mut usize, in_backticks: &mut bool) {
    for character in word.chars() {
        if character == '`' {
            *in_backticks = !*in_backticks;
        }
    }
    *depth = depth.saturating_add(word.matches("$(").count()).saturating_sub(word.matches(')').count());
}

fn shell_tokens(command: &str, split_equals: bool) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut token_started = false;
    let mut parameter_depth = 0_usize;
    let characters = command.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().copied().enumerate() {
        if escaped {
            token.push(character);
            token_started = true;
            escaped = false;
        } else if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            } else {
                token.push(character);
                token_started = true;
            }
        } else if quote == Some('"') {
            if character == '"' {
                quote = None;
            } else if character == '\\' {
                escaped = true;
            } else {
                token.push(character);
                token_started = true;
            }
        } else if matches!(character, '\'' | '"') {
            token_started = true;
            if character == '\'' && token.ends_with('$') {
                token.push(character);
            }
            quote = Some(character);
        } else if character == '\\' {
            escaped = true;
        } else if character == '$' && characters.get(index + 1) == Some(&'{') {
            parameter_depth += 1;
            token.push(character);
            token_started = true;
        } else if character == '}' && parameter_depth > 0 {
            parameter_depth -= 1;
            token.push(character);
            token_started = true;
        } else if parameter_depth == 0 && (character.is_whitespace() || split_equals && character == '=') {
            if token_started {
                tokens.push(std::mem::take(&mut token));
                token_started = false;
            }
        } else {
            token.push(character);
            token_started = true;
        }
    }
    if escaped {
        token.push('\\');
        token_started = true;
    }
    if token_started {
        tokens.push(token);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::{
        command_substitution_commands, command_tokens, declared_shell_function_count, declared_shell_functions, has_executable_unquoted_heredoc, shell_expansion_source,
        source_command_tokens, structured_source_commands, without_noncommand_shell_data,
    };

    #[test]
    fn shell_function_declarations_require_valid_names_and_braces() {
        let functions = declared_shell_functions(concat!("alpha() ", "{\n  :\n}\nfunction beta() ", "{\n  :\n}\nfunction gamma ", "{\n  :\n}\n"));
        assert_eq!(functions.into_iter().collect::<Vec<_>>(), ["alpha", "beta", "gamma"]);
        assert!(declared_shell_functions("9invalid() {\n}\nnot-a-name() {\n}\nname() not-a-body\nfunction\n").is_empty());
        assert_eq!(
            declared_shell_function_count(concat!("function alpha\n", "{\n  :\n}\nalpha()\n", "{\n  :\n}\n"), "alpha"),
            2
        );
    }

    #[test]
    fn ansi_c_quoted_words_retain_an_execution_marker() {
        assert_eq!(command_tokens("$'\\x73\\x68' quality/lint.txt"), vec!["$'\\x73\\x68", "quality/lint.txt"]);
        assert_eq!(command_tokens("printf '%s' \"$'\\x73'\""), vec!["printf", "%s", "$'x73'"]);
    }

    #[test]
    fn quoted_empty_arguments_keep_their_position() {
        assert_eq!(source_command_tokens("read -r -d '' value"), [vec!["read", "-r", "-d", "", "value"]]);
        assert_eq!(source_command_tokens("command \"\" tail"), [vec!["command", "", "tail"]]);
    }

    #[test]
    fn quoted_program_text_does_not_create_command_fragments() {
        let commands = source_command_tokens("digest=$(printf '%s' value | sed 's/../\\\\x&/g')\n/tmp/lint");
        assert!(!commands.iter().flatten().any(|token| token == "/g)"));
        assert!(commands.iter().any(|tokens| tokens.first().is_some_and(|token| token == "/tmp/lint")));
    }

    #[test]
    fn command_substitution_assignment_remains_one_command_segment() {
        assert_eq!(
            source_command_tokens(r#"tool=$(authenticated_tool "$tool" "$expected")"#),
            [vec!["tool=$(authenticated_tool", "$tool", "$expected)"]]
        );
        let (substitutions, malformed) = command_substitution_commands(r#"tool=$(authenticated_tool "$tool" "$expected")"#, true);
        assert!(!malformed);
        assert_eq!(substitutions, [r#"authenticated_tool "$tool" "$expected""#]);
        assert_eq!(source_command_tokens(&substitutions[0]), [vec!["authenticated_tool", "$tool", "$expected"]]);
    }

    #[test]
    fn parameter_expansion_diagnostics_remain_one_shell_word() {
        assert_eq!(
            source_command_tokens("root=${ROOT:-${HOME:?gate requires ROOT or HOME}/root}"),
            [vec!["root=${ROOT:-${HOME:?gate requires ROOT or HOME}/root}"]]
        );
    }

    #[test]
    fn file_descriptor_redirections_do_not_create_numeric_commands() {
        assert_eq!(source_command_tokens("printf value >&2"), [vec!["printf", "value", ">&2"]]);
        assert_eq!(source_command_tokens("command 2>&1"), [vec!["command", "2>&1"]]);
        assert_eq!(source_command_tokens("command &>output"), [vec!["command", "&>output"]]);
    }

    #[test]
    fn process_substitution_pipelines_remain_in_the_containing_command() {
        assert_eq!(
            source_command_tokens("read -r first < <(printf '%s\\n' value | tail -n1) second"),
            [vec!["read", "-r", "first", "<", "<(printf", "%s\\n", "value", "|", "tail", "-n1)", "second"]]
        );
    }

    #[test]
    fn conditional_operators_do_not_promote_substitutions_to_commands() {
        let commands = source_command_tokens(r#"if [[ ! -f $tool || $(sha256_file "$tool") != "$expected" ]]; then exit 1; fi"#);
        assert!(
            commands
                .iter()
                .any(|tokens| tokens.first().is_some_and(|token| token == "if") && tokens.iter().any(|token| token.contains("sha256_file")))
        );
        assert!(!commands.iter().any(|tokens| tokens.first().is_some_and(|token| token.starts_with("$(sha256_file"))));

        let arithmetic = source_command_tokens("if (( ready || fallback )); then true; fi");
        assert!(
            arithmetic
                .iter()
                .any(|tokens| tokens.first().is_some_and(|token| token == "if") && tokens.iter().any(|token| token == "fallback"))
        );
        assert!(!arithmetic.iter().any(|tokens| tokens.first().is_some_and(|token| token == "fallback")));

        let followed = source_command_tokens("if (( ready)); then sh quality/lint.txt; fi");
        assert!(
            followed
                .iter()
                .any(|tokens| tokens.first().is_some_and(|token| token == "then") && tokens.iter().any(|token| token == "sh"))
        );

        let quoted = source_command_tokens(r#"if (( ready + \"))\" )); then sh quality/lint.txt; fi"#);
        assert!(
            quoted
                .iter()
                .any(|tokens| tokens.first().is_some_and(|token| token == "then") && tokens.iter().any(|token| token == "sh"))
        );
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
    fn arithmetic_shifts_do_not_hide_following_commands() {
        for source in [
            "(( 1 << 2 ))\nquality/run-lints\n",
            "(( value <<= 1 ))\nquality/run-lints\n",
            "value=$((1 << 2))\nquality/run-lints\n",
            "value=$((\n  1 <<\n  2\n))\nquality/run-lints\n",
        ] {
            let normalized = without_noncommand_shell_data(source);
            assert!(normalized.contains("quality/run-lints"), "{source}");
            assert!(shell_expansion_source(source).contains("quality/run-lints"), "{source}");
        }
    }

    #[test]
    fn arithmetic_before_a_real_heredoc_preserves_both_contexts() {
        let source = "printf '%s' $((1 << 2)) <<'LITERAL'\n$(quality/ignored)\nLITERAL\nquality/run-lints\n";
        let normalized = without_noncommand_shell_data(source);
        assert!(!normalized.contains("quality/ignored"));
        assert!(normalized.contains("quality/run-lints"));
    }

    #[test]
    fn comments_cannot_open_heredocs_or_poison_later_arithmetic_state() {
        let fake_heredoc = "# <<NEVER\nquality/run-lints\n";
        assert!(without_noncommand_shell_data(fake_heredoc).contains("quality/run-lints"));

        let fake_arithmetic = "# ((1 <<\ncat <<'DOC'\n$(quality/ignored)\nDOC\nquality/run-lints\n";
        let normalized = without_noncommand_shell_data(fake_arithmetic);
        assert!(!normalized.contains("quality/ignored"));
        assert!(normalized.contains("quality/run-lints"));
        assert!(!has_executable_unquoted_heredoc(fake_arithmetic));
    }

    #[test]
    fn structured_commands_count_only_shell_syntax_braces() {
        let commands = structured_source_commands("check() {\n  printf '%s' '}'\n}\n");
        assert_eq!(commands.iter().map(|command| command.open_braces).sum::<usize>(), 1);
        assert_eq!(commands.iter().map(|command| command.close_braces).sum::<usize>(), 1);
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
    fn shell_expansion_source_removes_only_nonexpanding_heredoc_payloads() {
        let source = concat!(
            "cat <<'LITERAL'\n",
            "$((ignored))\n",
            "\\\n",
            "LITERAL\n",
            "cat <<ACTIVE\n",
            "'$((checked))' # quotes and comments are data here\n",
            "ACTIVE\n",
            "$((outside))\n",
        );
        let expansion_source = shell_expansion_source(source);
        assert!(!expansion_source.contains("ignored"));
        assert!(expansion_source.contains("$((checked))"));
        assert!(expansion_source.contains("$((outside))"));
        assert!(!expansion_source.contains("'$((checked))'"));
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

        let multiline = "case \"$name\" in\n  FIRST | SECOND | \\\n    THIRD | FOURTH) quality/check-case ;;\nesac\n";
        let normalized = without_noncommand_shell_data(multiline);
        assert!(!normalized.contains("FIRST"));
        assert!(!normalized.contains("THIRD"));
        assert!(normalized.contains("quality/check-case"));
    }

    #[test]
    fn here_strings_do_not_hide_following_commands_as_heredoc_payloads() {
        let source = "read -r value <<<\"$metadata\"\njust check-quality\n";
        assert_eq!(without_noncommand_shell_data(source), source);
    }
}
