pub(super) fn commands(source: &str) -> Vec<super::StructuredCommand> {
    let commands_only = super::without_noncommand_shell_data(source);
    let logical = commands_only.replace("\\\r\n", "").replace("\\\n", "");
    super::shell_command_segments(&logical)
        .into_iter()
        .map(|segment| {
            let source = super::command_without_comment(segment);
            let (open_braces, close_braces) = structural_braces(source);
            super::StructuredCommand {
                words: super::shell_tokens(source, false),
                open_braces,
                close_braces,
            }
        })
        .collect()
}

fn structural_braces(source: &str) -> (usize, usize) {
    let mut open = 0;
    let mut close = 0;
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
        } else if quote.is_none() && substitution_depth == 0 && structural_brace(source, index, previous) {
            open += usize::from(character == '{');
            close += usize::from(character == '}');
        }
        previous = Some(character);
    }
    (open, close)
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

fn structural_brace(source: &str, index: usize, previous: Option<char>) -> bool {
    let character = source[index..].chars().next().expect("character index");
    matches!(character, '{' | '}') && previous.is_none_or(brace_boundary) && source[index + character.len_utf8()..].chars().next().is_none_or(brace_boundary)
}

const fn brace_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')' | '<' | '>')
}
