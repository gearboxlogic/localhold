#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum CommandSeparator {
    Sequential,
    And,
    Or,
    Pipeline,
    Background,
    End,
}

pub(super) struct ShellCommandSegment<'a> {
    pub(super) source: &'a str,
    pub(super) conditionally_executed: bool,
    pub(super) isolated: bool,
    pub(super) following: CommandSeparator,
}

struct Collector<'a> {
    source: &'a str,
    segments: Vec<ShellCommandSegment<'a>>,
    start: usize,
    list_start: usize,
    conditional: bool,
    isolated: bool,
    awaiting_command: bool,
}

impl<'a> Collector<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            segments: Vec::new(),
            start: 0,
            list_start: 0,
            conditional: false,
            isolated: false,
            awaiting_command: false,
        }
    }

    const fn continuation_newline(&mut self, next_start: usize) {
        self.start = next_start;
    }

    fn push(&mut self, end: usize, next_start: usize, following: CommandSeparator, compound_start: Option<usize>) {
        let pipeline = following == CommandSeparator::Pipeline;
        let background = following == CommandSeparator::Background;
        if let Some(start) = compound_start {
            self.list_start = self.list_start.min(start);
        }
        self.segments.push(ShellCommandSegment {
            source: &self.source[self.start..end],
            conditionally_executed: self.conditional,
            isolated: self.isolated || pipeline || background,
            following,
        });
        if background {
            for segment in &mut self.segments[self.list_start..] {
                segment.isolated = true;
            }
        }
        self.start = next_start;
        self.conditional = matches!(following, CommandSeparator::And | CommandSeparator::Or);
        self.isolated = pipeline;
        self.awaiting_command = self.conditional || pipeline;
        if matches!(following, CommandSeparator::Sequential | CommandSeparator::Background) {
            self.list_start = self.segments.len();
        }
    }

    fn finish(mut self) -> Vec<ShellCommandSegment<'a>> {
        self.push(self.source.len(), self.source.len(), CommandSeparator::End, None);
        self.segments
    }
}

pub(super) fn shell_command_segments(source: &str) -> Vec<ShellCommandSegment<'_>> {
    let mut output = Collector::new(source);
    let mut state = ScanState::default();
    for (index, character) in source.char_indices() {
        if state.skip == Some(index) {
            state.skip = None;
            state.previous = Some(character);
            continue;
        }
        if state.comment {
            scan_comment(index, character, &mut state, &mut output);
        } else {
            scan_character(source, index, character, &mut state, &mut output);
        }
        state.previous = Some(character);
    }
    output.finish()
}

fn scan_comment(index: usize, character: char, state: &mut ScanState, output: &mut Collector<'_>) {
    if character != '\n' {
        return;
    }
    if output.awaiting_command {
        output.continuation_newline(index + 1);
    } else {
        output.push(index, index + 1, CommandSeparator::Sequential, state.pending_group_start.take());
    }
    state.comment = false;
}

#[derive(Default)]
struct ScanState {
    quote: Option<char>,
    escaped: bool,
    comment: bool,
    conditional: bool,
    substitution_depth: usize,
    group_starts: Vec<usize>,
    pending_group_start: Option<usize>,
    skip: Option<usize>,
    previous: Option<char>,
}

fn scan_character(source: &str, index: usize, character: char, state: &mut ScanState, output: &mut Collector<'_>) {
    if state.escaped {
        state.escaped = false;
    } else if character == '\\' && state.quote != Some('\'') {
        state.escaped = true;
    } else if matches!(character, '\'' | '"') {
        state.quote = updated_quote(state.quote, character);
    } else if comment_opener(character, state.quote, state.previous) {
        state.comment = true;
    } else if substitution_opener(source, index, character, state) {
        state.substitution_depth += 1;
    } else if state.quote.is_none() && state.substitution_depth > 0 && character == ')' {
        state.substitution_depth -= 1;
    } else if conditional_opener(source, index, state) {
        state.conditional = true;
    } else if conditional_closer(source, index, state) {
        state.conditional = false;
    } else if separator(source, index, character, state).is_some() {
        push_separator(source, index, character, state, output);
    } else if structural_group(source, index, character, state) {
        match character {
            '{' | '(' => state.group_starts.push(output.list_start),
            '}' | ')' => state.pending_group_start = state.group_starts.pop(),
            _ => {}
        }
    }
}

fn push_separator(source: &str, index: usize, character: char, state: &mut ScanState, output: &mut Collector<'_>) {
    let following = separator(source, index, character, state).expect("separator was checked");
    if character == '\n' && output.awaiting_command && output.source[output.start..index].trim().is_empty() {
        output.continuation_newline(index + 1);
        return;
    }
    let doubled = matches!(following, CommandSeparator::And | CommandSeparator::Or) || character == '|' && source[index + 1..].starts_with('&');
    let length = 1 + usize::from(doubled);
    output.push(index, index + length, following, state.pending_group_start.take());
    state.skip = doubled.then_some(index + 1);
}

fn separator(source: &str, index: usize, character: char, state: &ScanState) -> Option<CommandSeparator> {
    if state.quote.is_some() || state.substitution_depth > 0 || state.conditional {
        return None;
    }
    let following = &source[index + character.len_utf8()..];
    match character {
        '\n' | ';' => Some(CommandSeparator::Sequential),
        '&' if !matches!(state.previous, Some('<' | '>')) && !following.starts_with('>') => Some(if following.starts_with('&') {
            CommandSeparator::And
        } else {
            CommandSeparator::Background
        }),
        '|' if following.starts_with('|') => Some(CommandSeparator::Or),
        '|' => Some(CommandSeparator::Pipeline),
        _ => None,
    }
}

fn conditional_opener(source: &str, index: usize, state: &ScanState) -> bool {
    state.quote.is_none()
        && state.substitution_depth == 0
        && !state.conditional
        && (conditional_delimiter(source, index, state.previous, "[[") || conditional_delimiter(source, index, state.previous, "(("))
}

fn conditional_closer(source: &str, index: usize, state: &ScanState) -> bool {
    state.quote.is_none() && state.substitution_depth == 0 && state.conditional && (closer_delimiter(source, index, "]]") || closer_delimiter(source, index, "))"))
}

fn conditional_delimiter(source: &str, index: usize, previous: Option<char>, delimiter: &str) -> bool {
    source[index..].starts_with(delimiter) && previous.is_none_or(word_boundary) && source[index + delimiter.len()..].chars().next().is_none_or(word_boundary)
}

fn closer_delimiter(source: &str, index: usize, delimiter: &str) -> bool {
    source[index..].starts_with(delimiter) && source[index + delimiter.len()..].chars().next().is_none_or(word_boundary)
}

fn structural_group(source: &str, index: usize, character: char, state: &ScanState) -> bool {
    state.quote.is_none()
        && state.substitution_depth == 0
        && !state.conditional
        && matches!(character, '{' | '}' | '(' | ')')
        && state.previous.is_none_or(group_boundary)
        && source[index + 1..].chars().next().is_none_or(group_boundary)
}

fn substitution_opener(source: &str, index: usize, character: char, state: &ScanState) -> bool {
    state.quote.is_none()
        && (source[index..].starts_with("$(")
            || source[index..].starts_with("<(")
            || source[index..].starts_with(">(")
            || state.substitution_depth > 0 && character == '(' && !matches!(state.previous, Some('$' | '<' | '>')))
}

fn comment_opener(character: char, quote: Option<char>, previous: Option<char>) -> bool {
    quote.is_none() && character == '#' && previous.is_none_or(|value| value.is_whitespace() || matches!(value, ';' | '&' | '|'))
}

fn updated_quote(quote: Option<char>, character: char) -> Option<char> {
    if quote == Some(character) { None } else { quote.or(Some(character)) }
}

const fn word_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')')
}

const fn group_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')' | '<' | '>')
}
