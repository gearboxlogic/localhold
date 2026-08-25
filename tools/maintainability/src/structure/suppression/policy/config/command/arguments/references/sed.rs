use super::{SelectedInput, ValueSemantics};

const PROGRAM_FILES: ProgramFileSyntax = ProgramFileSyntax::new(&["--file"], &['f'], &['e', 'i'], &[]);

pub(super) fn program_with_semantics_is_opaque(path: &str, source_is_reviewed: bool, command: &str, arguments: &[String], semantics: ValueSemantics) -> bool {
    if super::mutation::reviewed_arguments(path, source_is_reviewed, command, arguments) {
        return false;
    }
    let (inputs, opaque) = program_file_inputs(arguments, &PROGRAM_FILES);
    opaque || !inputs.is_empty() || inline_program_is_opaque(arguments, semantics)
}

fn inline_program_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    let mut index = 0;
    let mut program_selected = false;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            index += 1;
            if !program_selected {
                return arguments.get(index).is_none_or(|program| program_can_execute(program));
            }
            break;
        }
        match expression(argument) {
            Expression::Following => {
                program_selected = true;
                index += 1;
                if arguments.get(index).is_none_or(|program| program_is_dynamic_or_can_execute(program, semantics)) {
                    return true;
                }
            }
            Expression::Attached(program) => {
                program_selected = true;
                if program_is_dynamic_or_can_execute(program, semantics) {
                    return true;
                }
            }
            Expression::Other if !argument.starts_with('-') && !program_selected => {
                return program_is_dynamic_or_can_execute(argument, semantics);
            }
            Expression::Other => {}
        }
        index += 1;
    }
    false
}

fn program_is_dynamic_or_can_execute(program: &str, semantics: ValueSemantics) -> bool {
    semantics.contains_dynamic(program) || program_can_execute(program)
}

enum Expression<'a> {
    Other,
    Following,
    Attached(&'a str),
}

fn expression(argument: &str) -> Expression<'_> {
    if argument == "-e" {
        return Expression::Following;
    }
    let (option, program) = argument.split_once('=').map_or((argument, None), |(option, program)| (option, Some(program)));
    if option.len() >= "--e".len() && "--expression".starts_with(option) {
        return program.map_or(Expression::Following, Expression::Attached);
    }
    let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) else {
        return Expression::Other;
    };
    let Some(index) = options.find('e') else {
        return Expression::Other;
    };
    let program = &options[index + 'e'.len_utf8()..];
    if program.is_empty() { Expression::Following } else { Expression::Attached(program) }
}

fn program_can_execute(program: &str) -> bool {
    std::iter::once(program)
        .chain(
            program
                .char_indices()
                .filter(|&(_, character)| matches!(character, ';' | '\n' | '{' | '}'))
                .map(|(offset, character)| &program[offset + character.len_utf8()..]),
        )
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .any(command_can_execute)
}

fn command_can_execute(command: &str) -> bool {
    let command = command_after_addresses(command);
    if command.starts_with(['e', 'w', 'W']) {
        return true;
    }
    command.strip_prefix('s').is_some_and(substitution_has_opaque_flag)
}

fn command_after_addresses(mut command: &str) -> &str {
    loop {
        command = command.trim_start();
        let address_length = command
            .chars()
            .take_while(|character| character.is_ascii_digit() || matches!(character, '$' | '+' | '-' | '~' | ','))
            .map(char::len_utf8)
            .sum::<usize>();
        if address_length > 0 {
            command = &command[address_length..];
            continue;
        }
        if let Some(end) = regex_address_end(command) {
            command = &command[end..];
            continue;
        }
        return command.trim_start_matches(['!', ' ', '\t']);
    }
}

fn regex_address_end(command: &str) -> Option<usize> {
    let mut characters = command.char_indices();
    let (_, first) = characters.next()?;
    let (delimiter_offset, delimiter) = if first == '/' {
        (0, first)
    } else if first == '\\' {
        characters.next()?
    } else {
        return None;
    };
    let content_start = delimiter_offset + delimiter.len_utf8();
    let mut escaped = false;
    for (offset, character) in command[content_start..].char_indices() {
        if character == delimiter && (delimiter == '\\' || !escaped) {
            let end = content_start + offset + character.len_utf8();
            let modifiers = command[end..]
                .chars()
                .take_while(|modifier| matches!(modifier, 'I' | 'M'))
                .map(char::len_utf8)
                .sum::<usize>();
            return Some(end + modifiers);
        }
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        }
    }
    None
}

fn substitution_has_opaque_flag(program: &str) -> bool {
    let Some(delimiter) = program.chars().next().filter(|character| !character.is_alphanumeric() && !character.is_whitespace()) else {
        return false;
    };
    let mut escaped = false;
    let mut delimiters = 0;
    for character in program[delimiter.len_utf8()..].chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == delimiter {
            delimiters += 1;
        } else if delimiters >= 2 && matches!(character, 'e' | 'w' | 'W') {
            return true;
        }
    }
    false
}

struct ProgramFileSyntax {
    long: &'static [&'static str],
    short: &'static [char],
    short_with_operand: &'static [char],
    opaque_short: &'static [char],
}

impl ProgramFileSyntax {
    const fn new(long: &'static [&'static str], short: &'static [char], short_with_operand: &'static [char], opaque_short: &'static [char]) -> Self {
        Self {
            long,
            short,
            short_with_operand,
            opaque_short,
        }
    }
}

fn program_file_inputs<'a>(arguments: &'a [String], syntax: &ProgramFileSyntax) -> (Vec<&'a str>, bool) {
    let mut inputs = Vec::new();
    let mut opaque = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            break;
        }
        let matched_long_option = syntax.long.iter().find(|option| argument == **option);
        let selected = if matched_long_option.is_some() {
            index += 1;
            program_file_input(arguments.get(index).map(String::as_str))
        } else if let Some(candidate) = syntax
            .long
            .iter()
            .find_map(|option| argument.strip_prefix(option).and_then(|candidate| candidate.strip_prefix('=')))
        {
            program_file_input(Some(candidate))
        } else if abbreviated_long_program_file_option(argument, syntax) {
            SelectedInput::Opaque
        } else {
            let (selected, consumes_following) = short_program_file_input(argument, arguments.get(index + 1).map(String::as_str), syntax);
            if consumes_following {
                index += 1;
            }
            selected
        };
        match selected {
            SelectedInput::None => {}
            SelectedInput::Literal(candidate) => inputs.push(candidate),
            SelectedInput::Opaque => opaque = true,
        }
        index += 1;
    }
    (inputs, opaque)
}

fn abbreviated_long_program_file_option(argument: &str, syntax: &ProgramFileSyntax) -> bool {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    name.len() > 2 && name.starts_with("--") && syntax.long.iter().any(|option| option.starts_with(name))
}

fn program_file_input(candidate: Option<&str>) -> SelectedInput<'_> {
    match candidate {
        Some(candidate) if !candidate.is_empty() && candidate != "-" => SelectedInput::Literal(candidate),
        Some(_) | None => SelectedInput::Opaque,
    }
}

fn short_program_file_input<'a>(argument: &'a str, following: Option<&'a str>, syntax: &ProgramFileSyntax) -> (SelectedInput<'a>, bool) {
    let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) else {
        return (SelectedInput::None, false);
    };
    for (index, option) in options.char_indices() {
        if syntax.opaque_short.contains(&option) {
            return (SelectedInput::Opaque, false);
        }
        if syntax.short_with_operand.contains(&option) {
            return (SelectedInput::None, false);
        }
        if syntax.short.contains(&option) {
            let candidate = &options[index + option.len_utf8()..];
            return if candidate.is_empty() {
                (program_file_input(following), following.is_some())
            } else {
                (program_file_input(Some(candidate)), false)
            };
        }
    }
    (SelectedInput::None, false)
}

#[cfg(test)]
mod tests {
    use super::program_can_execute;

    #[test]
    fn alternate_and_modified_regex_addresses_preserve_executable_commands() {
        for program in [
            r"\%foo%e sh quality/hidden.txt",
            r"\#foo#e sh quality/hidden.txt",
            r"\#foo\#bar#e sh quality/hidden.txt",
            r"\;foo;e sh quality/hidden.txt",
            r"\\foo\e sh quality/hidden.txt",
            r"1,\#foo#e sh quality/hidden.txt",
            r"\#x#,\#foo#e sh quality/hidden.txt",
            r"\#foo#,+2e sh quality/hidden.txt",
            r"\#foo#,~2e sh quality/hidden.txt",
            r"\#foo#IM! e sh quality/hidden.txt",
            r"/foo;bar/e sh quality/hidden.txt",
            r"/foo/Ie sh quality/hidden.txt",
            r"p;\#foo;bar#e sh quality/hidden.txt",
        ] {
            assert!(program_can_execute(program), "{program}");
        }
    }

    #[test]
    fn benign_regex_addresses_and_substitutions_remain_non_executable() {
        for program in [
            r"\#foo#p",
            r"\#foo\#bar#p",
            r"\#foo#Ip",
            r"1,\#foo#p",
            r"\#x#,\#foo#p",
            r"/foo;bar/p",
            r"p;\#foo;bar#p",
            r"s;a;b;g",
        ] {
            assert!(!program_can_execute(program), "{program}");
        }
    }
}
