use std::iter::Peekable;
use std::str::Chars;

mod lexical;
mod reviewed;
use lexical::{has_code_semicolon, has_scriptblock_parameter_command};

#[derive(Clone, Copy)]
enum State {
    Code,
    SingleQuoted,
    DoubleQuoted,
    SingleHereString,
    DoubleHereString,
    LineComment,
    BlockComment,
}

pub(super) struct Analysis {
    pub(super) commands: String,
    pub(super) unresolved_statements: Vec<String>,
}

impl Analysis {
    pub(super) const fn unresolved(&self) -> bool {
        !self.unresolved_statements.is_empty()
    }
}

pub(super) fn unresolved_is_reviewed(path: &str, source_is_reviewed: bool, analysis: &Analysis) -> bool {
    reviewed::accepts(path, source_is_reviewed, analysis)
}

pub(super) fn normalize_escapes(source: &str) -> String {
    let mut normalized = String::with_capacity(source.len());
    let mut characters = source.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '`' {
            push_escaped(&mut normalized, &mut characters, &mut line_prefix_is_whitespace);
            continue;
        }

        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    normalized.push(character);
                    update_line_prefix(character, &mut line_prefix_is_whitespace);
                    normalized.push(characters.next().expect("peeked quote"));
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }

        normalized.push(character);
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    normalized
}

pub(super) fn analyze_execution_commands(source: &str) -> Analysis {
    let mut commands = Vec::new();
    let mut unresolved_statements = Vec::new();
    for statement in native_statements(source) {
        let (command, unresolved) = analyze_statement(&statement, &normalize_escapes(&statement));
        commands.push(command);
        if unresolved {
            unresolved_statements.push(statement);
        }
    }
    if has_code_semicolon(source) {
        unresolved_statements.push(source.trim().to_owned());
    }
    Analysis {
        commands: commands.join("\n"),
        unresolved_statements,
    }
}

#[cfg(test)]
pub(super) fn normalize_execution_commands(source: &str) -> String {
    analyze_execution_commands(source).commands
}

fn analyze_statement(raw: &str, normalized: &str) -> (String, bool) {
    if is_native_exit_status_guard(normalized) {
        return (":".to_owned(), false);
    }
    if powershell_only_control_flow(normalized) {
        return (":".to_owned(), control_flow_requires_review(raw));
    }
    if let Some((_, expression)) = normalized.split_once(" = ").filter(|(target, _)| target.trim_start().starts_with('$')) {
        let expression = expression.trim_start();
        let raw_expression = raw.split_once(" = ").map_or(raw, |(_, expression)| expression.trim_start());
        if let Some(native) = assigned_native_command(expression) {
            return (
                native.to_owned(),
                has_structural_execution(raw_expression) || has_scriptblock_parameter_command(raw_expression),
            );
        }
        return (":".to_owned(), !inert_assignment_is_proven(raw_expression));
    }
    if let Some(command) = literal_call_operator_command(normalized) {
        return (
            command.to_owned(),
            has_structural_execution(command) || command.contains('$') || has_scriptblock_parameter_command(raw),
        );
    }
    let unresolved = has_structural_execution(raw) || has_scriptblock_parameter_command(raw);
    (if unresolved { ":".to_owned() } else { normalized.to_owned() }, unresolved)
}

fn inert_assignment_is_proven(expression: &str) -> bool {
    if !inert_assignment_expression(expression) {
        return false;
    }
    if !has_structural_execution(expression) && !has_scriptblock_parameter_command(expression) {
        return true;
    }
    let expression = expression.trim();
    !["$(", "|", ";", "&", "{", ".Invoke("].iter().any(|token| expression.contains(token))
        && expression.matches('(').count() == 1
        && (expression.starts_with("(Get-FileHash ") || expression.starts_with("[IO.Path]::GetFileNameWithoutExtension("))
}

fn has_structural_execution(source: &str) -> bool {
    let mut characters = source.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '`' {
            let escaped = characters.next();
            if let Some(escaped) = escaped {
                update_line_prefix(escaped, &mut line_prefix_is_whitespace);
            }
            continue;
        }
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '$' && characters.peek() == Some(&'(') {
            return true;
        }
        if matches!(state, State::Code) && matches!(character, '(' | '{' | ';' | '|' | '&' | '>') {
            return true;
        }
        if matches!(state, State::Code) && character == '.' && characters.peek().is_some_and(|next| next.is_whitespace()) {
            return true;
        }
        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    characters.next();
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => state = State::Code,
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => state = State::Code,
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    false
}

fn powershell_only_control_flow(line: &str) -> bool {
    let line = line.trim().to_ascii_lowercase();
    line == "}"
        || line.starts_with("throw ")
        || (line.starts_with("if (") || line.starts_with("foreach (")) && (line.ends_with('{') || line.contains("{ throw "))
        || line.strip_prefix('$').is_some_and(|value| {
            value
                .strip_suffix("++")
                .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        })
}

fn control_flow_requires_review(statement: &str) -> bool {
    let statement = statement.trim();
    if statement == "}"
        || statement.strip_prefix('$').is_some_and(|value| {
            value
                .strip_suffix("++")
                .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        })
    {
        return false;
    }
    if statement.to_ascii_lowercase().starts_with("throw ") {
        return has_structural_execution(statement) || has_scriptblock_parameter_command(statement);
    }
    true
}

fn assigned_native_command(command: &str) -> Option<&str> {
    let command = command.trim();
    let candidate = if command.starts_with("./") || command.starts_with(r".\") {
        command
    } else {
        command.split('|').map(str::trim).find(|segment| segment.starts_with("./") || segment.starts_with(r".\"))?
    };
    Some(candidate.strip_suffix(')').unwrap_or(candidate).trim_end())
}

fn inert_assignment_expression(command: &str) -> bool {
    let command = command.trim_start_matches(['(', '@']).trim_start();
    command.starts_with(['\'', '"', '$'])
        || command.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || command.starts_with("[IO.Path]::GetFileNameWithoutExtension(")
        || ["ConvertFrom-Json", "Get-ChildItem", "Get-Content", "Get-FileHash", "Join-Path", "Select-Object"]
            .iter()
            .any(|cmdlet| command.strip_prefix(cmdlet).is_some_and(|suffix| suffix.starts_with(char::is_whitespace)))
}

fn is_native_exit_status_guard(line: &str) -> bool {
    matches!(
        line.chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
            .as_str(),
        "exit$lastexitcode" | "if($lastexitcode-ne0){exit$lastexitcode}" | "if($lastexitcode){exit$lastexitcode}"
    )
}

fn literal_call_operator_command(line: &str) -> Option<&str> {
    let command = line.trim_start().strip_prefix('&')?.trim_start();
    let program = command.split_ascii_whitespace().next()?;
    (!program.starts_with(['$', '(', '{', '\'', '"']) && !program.contains(['*', '?', '[', '`'])).then_some(command)
}

pub(super) fn has_constructed_rust_arguments(source: &str) -> bool {
    let source = normalize_escapes(source);
    has_opaque_dispatch(&source) || source.split(['\n', ';', '|']).any(|command| has_rust_tool(command) && has_argument_expression(command))
}

pub(super) fn has_unchecked_native_quality_command(source: &str) -> bool {
    let statements = native_statements(source).into_iter().map(|statement| normalize_escapes(&statement)).collect::<Vec<_>>();
    let mut index = 0;
    while let Some(statement) = statements.get(index) {
        if !super::integrity::contains_quality_command(statement, true) {
            index += 1;
            continue;
        }
        let Some(check) = statements.get(index + 1) else {
            return true;
        };
        let check = check.chars().filter(|character| !character.is_whitespace()).collect::<String>().to_ascii_lowercase();
        if check == "exit$lastexitcode" {
            return false;
        }
        if !matches!(check.as_str(), "if($lastexitcode-ne0){exit$lastexitcode}" | "if($lastexitcode){exit$lastexitcode}") {
            return true;
        }
        index += 2;
    }
    false
}

fn native_statements(source: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut statement = String::new();
    let mut characters = source.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '`' {
            push_raw_escape(&mut statement, &mut characters, &mut line_prefix_is_whitespace);
            continue;
        }
        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => {
                    characters.next();
                    state = State::BlockComment;
                }
                '\'' => {
                    statement.push(character);
                    state = State::SingleQuoted;
                }
                '"' => {
                    statement.push(character);
                    state = State::DoubleQuoted;
                }
                '@' => {
                    statement.push(character);
                    if let Some(quote) = here_string_quote(&mut characters) {
                        state = here_string_state(quote);
                    }
                }
                ';' | '\n' => push_statement(&mut statements, &mut statement),
                _ => statement.push(character),
            },
            State::SingleQuoted if single_quoted_value_ends(character, &mut characters, &mut statement) => state = State::Code,
            State::DoubleQuoted => {
                statement.push(character);
                if character == '"' {
                    state = State::Code;
                }
            }
            State::SingleHereString | State::DoubleHereString => {
                statement.push(character);
                let closing_quote = if matches!(state, State::SingleHereString) { '\'' } else { '"' };
                if line_prefix_is_whitespace && character == closing_quote && characters.peek() == Some(&'@') {
                    statement.push(characters.next().expect("peeked here-string terminator"));
                    state = State::Code;
                }
            }
            State::LineComment if character == '\n' => {
                state = State::Code;
                push_statement(&mut statements, &mut statement);
            }
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => {
                characters.next();
                state = State::Code;
            }
            State::SingleQuoted | State::LineComment | State::BlockComment => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    push_statement(&mut statements, &mut statement);
    statements
}

fn push_raw_escape(statement: &mut String, characters: &mut Peekable<Chars<'_>>, line_prefix_is_whitespace: &mut bool) {
    statement.push('`');
    match characters.next() {
        Some('\r') if characters.peek() == Some(&'\n') => {
            statement.push('\r');
            statement.push(characters.next().expect("peeked escaped line feed"));
        }
        Some('\n') => statement.push('\n'),
        Some(escaped) => {
            statement.push(escaped);
            update_line_prefix(escaped, line_prefix_is_whitespace);
        }
        None => {}
    }
}

fn push_statement(statements: &mut Vec<String>, statement: &mut String) {
    let trimmed = statement.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_owned());
    }
    statement.clear();
}

fn has_opaque_dispatch(source: &str) -> bool {
    let mut characters = source.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    let mut word = String::new();
    let mut saw_script_block_type = false;
    let mut saw_io_type = false;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code) && (character.is_alphanumeric() || matches!(character, '-' | '_' | '.')) {
            word.push(character.to_ascii_lowercase());
            update_line_prefix(character, &mut line_prefix_is_whitespace);
            continue;
        }
        if matches!(state, State::Code) && !word.is_empty() {
            if process_api_word(&word) || saw_script_block_type && word == "create" || saw_io_type {
                return true;
            }
            saw_script_block_type = script_block_type_word(&word);
            saw_io_type = opaque_io_type_word(&word);
        }
        word.clear();
        match state {
            State::Code => match character {
                '#' => {
                    if line_prefix_is_whitespace && starts_requires_directive(characters.clone()) {
                        return true;
                    }
                    state = State::LineComment;
                }
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                '&' if characters.peek() == Some(&'&') => {
                    characters.next();
                }
                '&' if dynamic_call_operator(characters.clone()) => return true,
                ';' | '\n' | '|' => {
                    saw_script_block_type = false;
                    saw_io_type = false;
                }
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    characters.next();
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => {
                state = State::Code;
            }
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    matches!(state, State::Code) && (process_api_word(&word) || saw_script_block_type && word == "create" || saw_io_type && !word.is_empty())
}

fn starts_requires_directive(mut characters: Peekable<Chars<'_>>) -> bool {
    let mut directive = String::new();
    while characters.peek().is_some_and(|character| character.is_ascii_alphabetic()) {
        directive.push(characters.next().expect("peeked directive character").to_ascii_lowercase());
    }
    directive == "requires" && characters.peek().is_none_or(|character| character.is_whitespace())
}

fn opaque_io_type_word(word: &str) -> bool {
    let word = word.strip_prefix("system.").unwrap_or(word);
    word.starts_with("io.") && word != "io.path"
}

fn process_api_word(word: &str) -> bool {
    matches!(
        word,
        "new-alias"
            | "nal"
            | "set-alias"
            | "sal"
            | "start-process"
            | "saps"
            | "invoke-expression"
            | "iex"
            | "system.diagnostics.process"
            | "diagnostics.process"
            | "invoke-cimmethod"
            | "icim"
            | "invoke-wmimethod"
            | "iwmi"
            | "get-ciminstance"
            | "gcim"
            | "get-wmiobject"
            | "gwmi"
    )
}

fn script_block_type_word(word: &str) -> bool {
    matches!(
        word,
        "scriptblock" | "system.management.automation.scriptblock" | "wmi" | "wmiclass" | "cimclass" | "management.managementclass" | "system.management.managementclass"
    )
}

fn dynamic_call_operator(characters: Peekable<Chars<'_>>) -> bool {
    let command = call_operator_command(characters);
    let command = super::tokens::command_without_comment(&command);
    let tokens = super::tokens::command_tokens(command);
    let Some(command) = tokens.first() else {
        return false;
    };
    if dynamic_call_operand(command) {
        return true;
    }
    let Some(tool_index) = tokens.iter().position(|token| super::is_rust_tool_token(token, true)) else {
        return false;
    };
    tokens.iter().skip(tool_index + 1).any(|token| dynamic_call_operand(token))
}

fn call_operator_command(mut characters: Peekable<Chars<'_>>) -> String {
    let mut command = String::new();
    let mut quote = None;
    while let Some(character) = characters.next() {
        if quote == Some('\'') {
            if single_quoted_value_ends(character, &mut characters, &mut command) {
                quote = None;
            }
            continue;
        }
        if quote == Some('"') {
            command.push(character);
            if character == '"' {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            command.push(character);
        } else if character == '#' || matches!(character, '\n' | ';' | '|' | '&') {
            break;
        } else {
            command.push(character);
        }
    }
    command
}

fn single_quoted_value_ends(character: char, characters: &mut Peekable<Chars<'_>>, command: &mut String) -> bool {
    command.push(character);
    if character != '\'' {
        return false;
    }
    if characters.next_if_eq(&'\'').is_none() {
        return true;
    }
    command.push('\'');
    false
}

fn dynamic_call_operand(token: &str) -> bool {
    token.contains('$') || token.starts_with('@') || token.contains(['(', '{', '['])
}

fn has_rust_tool(command: &str) -> bool {
    super::tokens::command_tokens(command).iter().any(|token| super::is_rust_tool_token(token, true))
}

fn has_argument_expression(command: &str) -> bool {
    let mut characters = command.chars().peekable();
    let mut state = State::Code;
    let mut line_prefix_is_whitespace = true;
    while let Some(character) = characters.next() {
        if matches!(state, State::Code | State::DoubleQuoted | State::DoubleHereString) && character == '`' {
            characters.next();
            continue;
        }
        if matches!(state, State::Code) && matches!(character, '(' | '{') {
            return true;
        }
        if matches!(state, State::DoubleQuoted | State::DoubleHereString) && character == '$' && characters.peek() == Some(&'(') {
            return true;
        }
        match state {
            State::Code => match character {
                '#' => state = State::LineComment,
                '<' if characters.peek() == Some(&'#') => state = State::BlockComment,
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '@' => state = here_string_quote(&mut characters).map_or(State::Code, here_string_state),
                _ => {}
            },
            State::SingleQuoted if character == '\'' => {
                if characters.peek() == Some(&'\'') {
                    characters.next();
                    continue;
                }
                state = State::Code;
            }
            State::DoubleQuoted if character == '"' => state = State::Code,
            State::SingleHereString if line_prefix_is_whitespace && character == '\'' && characters.peek() == Some(&'@') => state = State::Code,
            State::DoubleHereString if line_prefix_is_whitespace && character == '"' && characters.peek() == Some(&'@') => state = State::Code,
            State::LineComment if character == '\n' => state = State::Code,
            State::BlockComment if character == '#' && characters.peek() == Some(&'>') => state = State::Code,
            _ => {}
        }
        update_line_prefix(character, &mut line_prefix_is_whitespace);
    }
    false
}

fn here_string_quote(characters: &mut Peekable<Chars<'_>>) -> Option<char> {
    let quote = characters.peek().copied().filter(|character| matches!(character, '\'' | '"'))?;
    characters
        .clone()
        .skip(1)
        .take_while(|character| *character != '\n')
        .all(char::is_whitespace)
        .then_some(quote)
}

const fn here_string_state(quote: char) -> State {
    if quote == '\'' { State::SingleHereString } else { State::DoubleHereString }
}

fn push_escaped(normalized: &mut String, characters: &mut Peekable<Chars<'_>>, line_prefix_is_whitespace: &mut bool) {
    match characters.next() {
        Some('\r') if characters.peek() == Some(&'\n') => {
            characters.next();
        }
        Some('\n') | None => {}
        Some(escaped) => {
            normalized.push(escaped);
            update_line_prefix(escaped, line_prefix_is_whitespace);
        }
    }
}

const fn update_line_prefix(character: char, line_prefix_is_whitespace: &mut bool) {
    if character == '\n' {
        *line_prefix_is_whitespace = true;
    } else if !character.is_whitespace() {
        *line_prefix_is_whitespace = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_execution_commands, has_constructed_rust_arguments, has_unchecked_native_quality_command, normalize_escapes, normalize_execution_commands};

    #[test]
    fn nested_execution_in_discardable_forms_is_unresolved() {
        for source in [
            "$value = $(./quality/payload.ps1)",
            "$value = \"prefix $(./quality/payload.ps1)\"",
            "$value = (./quality/payload.ps1)",
            "$value = & { ./quality/payload.ps1 }",
            "$value = . ./quality/payload.ps1",
            "$value = Get-Content input | ./quality/payload.ps1",
            "$value = $items | ForEach-Object { ./quality/payload.ps1 }",
            "$value = @($request | ./reviewed.exe | & $tool)",
            "$block = { ./quality/payload.ps1 }; $block.Invoke()",
            "if ($(./quality/payload.ps1)) { throw 'x' }",
            "if (& ./quality/payload.ps1) { throw 'x' }",
            "foreach ($item in (./quality/payload.ps1)) {}",
            "throw $(./quality/payload.ps1)",
            "$value = 'inert'; & $tool",
            "throw 'x'; & $tool",
        ] {
            assert!(analyze_execution_commands(source).unresolved(), "{source}");
        }
    }

    #[test]
    fn quoted_structural_tokens_are_inert_but_expanding_subexpressions_are_not() {
        for source in [
            "$value = '$(./quality/payload.ps1) | & . { }'",
            "# $(./quality/payload.ps1) | & . { }",
            "$value = @'\n$(./quality/payload.ps1) | & . { }\n'@",
            "$value = \"literal `$(` escaped\"",
        ] {
            assert!(!analyze_execution_commands(source).unresolved(), "{source}");
        }
        for source in ["$value = \"$(./quality/payload.ps1)\"", "$value = @\"\n$(./quality/payload.ps1)\n\"@"] {
            assert!(analyze_execution_commands(source).unresolved(), "{source}");
        }
    }

    #[test]
    fn lexical_statements_do_not_invent_commands_from_comments_or_literal_here_strings() {
        let analysis = analyze_execution_commands("$literal = @'\n./quality/literal.ps1\n'@\n<#\n./quality/comment.ps1\n#>\nWrite-Output safe\n");
        assert!(!analysis.unresolved());
        assert_eq!(analysis.commands, ":\nWrite-Output safe");

        let expanding = analyze_execution_commands("$value = @\"\n$(./quality/payload.ps1)\n\"@");
        assert!(expanding.unresolved());
        assert_eq!(expanding.unresolved_statements.len(), 1);
    }

    #[test]
    fn escaped_comment_and_quote_tokens_cannot_hide_execution() {
        for source in [
            "Write-Output `# | ./quality/payload.ps1",
            "$value = \"literal `\" # $(./quality/payload.ps1)\"",
            "Write-Output ready `\n| ./quality/payload.ps1",
        ] {
            let analysis = analyze_execution_commands(source);
            assert!(analysis.unresolved(), "{source}: {}", analysis.commands);
            assert_eq!(analysis.unresolved_statements.len(), 1, "{source}");
        }
    }

    #[test]
    fn literal_call_operators_and_repository_program_assignments_are_normalized() {
        assert_eq!(normalize_execution_commands("& cargo clippy -- -D warnings"), "cargo clippy -- -D warnings");
        assert_eq!(normalize_execution_commands("$binary = ./hold-smoke.exe --version"), "./hold-smoke.exe --version");
        assert_eq!(normalize_execution_commands("$binary = 'dist/hold.exe'\n$expected = 'hold 1.0'"), ":\n:");
        assert_eq!(normalize_execution_commands("if ($actual -ne $expected) { throw 'unexpected version' }"), ":");
        assert_eq!(normalize_execution_commands("foreach ($line in Get-Content sums) {\n$verified++\n}"), ":\n:\n:");
        assert_eq!(
            normalize_execution_commands("$responseLines = @($request | ./hold-smoke.exe 2>error.log)"),
            "./hold-smoke.exe 2>error.log"
        );
        assert_eq!(normalize_execution_commands("$value = unknown-command --flag"), ":");
        assert!(analyze_execution_commands("$value = unknown-command --flag").unresolved());
        assert_eq!(normalize_execution_commands("& $tool $arguments"), ":");
        assert!(analyze_execution_commands("& $tool $arguments").unresolved());
        assert_eq!(
            normalize_execution_commands("& cargo clippy -- -D warnings\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }"),
            "cargo clippy -- -D warnings\n:"
        );
        assert_eq!(normalize_execution_commands("exit $LASTEXITCODE"), ":");
    }

    #[test]
    fn escapes_are_normalized_only_where_powershell_expands_them() {
        assert_eq!(normalize_escapes("ca`rgo --a`llow"), "cargo --allow");
        assert_eq!(normalize_escapes("\"ca`rgo\""), "\"cargo\"");
        assert_eq!(normalize_escapes("'ca`rgo'"), "'ca`rgo'");
        assert_eq!(normalize_escapes("# ca`rgo\nca`rgo"), "# ca`rgo\ncargo");
        assert_eq!(normalize_escapes("<# ca`rgo #>\nca`rgo"), "<# ca`rgo #>\ncargo");
    }

    #[test]
    fn here_strings_keep_their_distinct_escape_semantics() {
        assert_eq!(normalize_escapes("@'\nca`rgo\n'@\nca`rgo"), "@'\nca`rgo\n'@\ncargo");
        assert_eq!(normalize_escapes("@\"\nca`rgo\n\"@\nca`rgo"), "@\"\ncargo\n\"@\ncargo");
    }

    #[test]
    fn constructed_rust_arguments_fail_closed() {
        assert!(has_constructed_rust_arguments("cargo clippy -- ('-' + 'A') warnings"));
        assert!(has_constructed_rust_arguments("cargo clippy -- \"$(Get-LintLevel)\" warnings"));
        assert!(has_constructed_rust_arguments(
            "Start-Process -FilePath ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('Y2FyZ28='))) -ArgumentList 'clippy','--','-A','warnings' -Wait"
        ));
        assert!(has_constructed_rust_arguments("[System.Diagnostics.Process]::Start($tool, $arguments).WaitForExit()"));
        assert!(has_constructed_rust_arguments("[Diagnostics.Process]::new()"));
        assert!(has_constructed_rust_arguments("[scriptblock]::Create($source).Invoke()"));
        assert!(has_constructed_rust_arguments("[System.Management.Automation.ScriptBlock]::Create($source).Invoke()"));
        assert!(analyze_execution_commands("$assembly = [System.Reflection.Assembly]::LoadFrom('quality/payload.dll')\n$assembly.EntryPoint.Invoke($null, @())").unresolved());
        assert!(has_constructed_rust_arguments("#Requires -Modules ./quality/payload.dll\nWrite-Output safe"));
        assert!(has_constructed_rust_arguments("#requires -PSSnapin Untrusted.SnapIn\nWrite-Output safe"));
        assert!(has_constructed_rust_arguments("[System.IO.File]::Copy('quality/Justfile', 'Justfile', $true)"));
        assert!(has_constructed_rust_arguments("[IO.Compression.ZipFile]::ExtractToDirectory($archive, '.')"));
        assert!(has_constructed_rust_arguments("New-Alias x ('Invoke-' + 'Expression'); x $decoded"));
        assert!(has_constructed_rust_arguments("Set-Alias x Invoke-Expression; x $decoded"));
        assert!(!has_constructed_rust_arguments("$scriptblock = 'inert'; Write-Output $scriptblock"));
        assert!(!has_constructed_rust_arguments("Write-Output '[scriptblock]::Create($source)'"));
        assert!(!has_constructed_rust_arguments("cargo clippy -- '-A' warnings"));
        assert!(!has_constructed_rust_arguments("Write-Output '(cargo clippy -- -A warnings)'"));
        assert!(!has_constructed_rust_arguments("Write-Output 'Start-Process cargo'"));
        assert!(!has_constructed_rust_arguments("Write-Output '[System.Diagnostics.Process]::Start($tool)'"));
        assert!(!has_constructed_rust_arguments("Write-Output 'New-Alias x Invoke-Expression'"));
        assert!(!has_constructed_rust_arguments("# Start-Process cargo"));
        assert!(!has_constructed_rust_arguments("# [System.Diagnostics.Process]::Start($tool)"));
        assert!(!has_constructed_rust_arguments("# Requires -Modules ./quality/payload.dll"));
        assert!(!has_constructed_rust_arguments("Write-Output '#Requires -Modules ./quality/payload.dll'"));
        assert!(!has_constructed_rust_arguments("@'\n#Requires -Modules ./quality/payload.dll\n'@"));
        assert!(!has_constructed_rust_arguments("@'\nStart-Process cargo\n'@"));
        assert!(!has_constructed_rust_arguments("@'\n[System.Diagnostics.Process]::Start($tool)\n'@"));
        assert!(!has_constructed_rust_arguments("$actual = (Get-FileHash -Algorithm SHA256 $path).Hash"));
        assert!(!has_constructed_rust_arguments("$name = [IO.Path]::GetFileNameWithoutExtension($archive.Name)"));
        assert!(!has_constructed_rust_arguments("Write-Output '[System.IO.File]::Copy($source, $destination)'"));
        assert!(!has_constructed_rust_arguments("# [System.IO.File]::Copy($source, $destination)"));
    }

    #[test]
    fn dynamic_call_operator_dispatch_fails_closed() {
        assert!(has_constructed_rust_arguments("& $tool $subcommand -- $flag warnings"));
        assert!(has_constructed_rust_arguments("& cargo $subcommand -- $flag warnings"));
        assert!(has_constructed_rust_arguments("& (Get-Command cargo) clippy -- -D warnings"));
        assert!(!has_constructed_rust_arguments("& cargo clippy -- -D warnings"));
        assert!(!has_constructed_rust_arguments("Write-Output '& $tool $flag'"));
        assert!(!has_constructed_rust_arguments("# & $tool $flag"));
        assert!(!has_constructed_rust_arguments("@'\n& $tool $flag\n'@"));
    }

    #[test]
    fn cim_and_wmi_process_dispatch_fail_closed() {
        for source in [
            "Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine=$command}",
            "icim -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine=$command}",
            "Invoke-WmiMethod -Class Win32_Process -Name Create -ArgumentList $command",
            "iwmi -Class Win32_Process -Name Create -ArgumentList $command",
            "(Get-CimInstance -ClassName Win32_Process).CimClass.CimClassMethods['Create']",
            "(gwmi -Class Win32_Process).Create($command)",
            "[wmiclass]'Win32_Process'::Create($command)",
            "[System.Management.ManagementClass]'Win32_Process'::Create($command)",
        ] {
            assert!(has_constructed_rust_arguments(source), "{source}");
        }
        for source in [
            "Write-Output 'Invoke-CimMethod -ClassName Win32_Process -MethodName Create'",
            "# Invoke-WmiMethod -Class Win32_Process -Name Create",
            "@'\n[wmiclass] Win32_Process :: Create\n'@",
        ] {
            assert!(!has_constructed_rust_arguments(source), "{source}");
        }
    }

    #[test]
    fn native_quality_commands_require_immediate_exit_status_enforcement() {
        assert!(has_unchecked_native_quality_command("cargo clippy --locked -- -D warnings\nexit 0"));
        assert!(has_unchecked_native_quality_command("cargo test --locked\nWrite-Output accepted"));
        assert!(has_unchecked_native_quality_command("cargo test --locked"));
        assert!(!has_unchecked_native_quality_command(
            "cargo clippy --locked -- -D warnings\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\nWrite-Output accepted"
        ));
        assert!(!has_unchecked_native_quality_command("cargo test --locked; exit $LASTEXITCODE"));
        assert!(!has_unchecked_native_quality_command("Write-Output 'cargo test --locked; exit 0'"));
        assert!(!has_unchecked_native_quality_command("# cargo test --locked\nWrite-Output accepted"));
    }
}
