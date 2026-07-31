use super::SelectedInput;

const PROGRAM_FILES: ProgramFileSyntax = ProgramFileSyntax::new(&["--file"], &['f'], &['e', 'i'], &[]);

pub(super) fn program_is_opaque(arguments: &[String]) -> bool {
    let (inputs, opaque) = program_file_inputs(arguments, &PROGRAM_FILES);
    opaque || !inputs.is_empty() || inline_program_is_opaque(arguments)
}

fn inline_program_is_opaque(arguments: &[String]) -> bool {
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
                if arguments.get(index).is_none_or(|program| program_can_execute(program)) {
                    return true;
                }
            }
            Expression::Attached(program) => {
                program_selected = true;
                if program_can_execute(program) {
                    return true;
                }
            }
            Expression::Other if !argument.starts_with('-') && !program_selected => return program_can_execute(argument),
            Expression::Other => {}
        }
        index += 1;
    }
    false
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
    program
        .split([';', '\n', '{', '}'])
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .any(command_can_execute)
}

fn command_can_execute(command: &str) -> bool {
    let command = command_after_addresses(command);
    if command.starts_with('e') {
        return true;
    }
    command.strip_prefix('s').is_some_and(substitution_has_execute_flag)
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
    let delimiter = command.strip_prefix('/').map(|_| '/')?;
    let mut escaped = false;
    for (offset, character) in command[delimiter.len_utf8()..].char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == delimiter {
            return Some(delimiter.len_utf8() + offset + character.len_utf8());
        }
    }
    None
}

fn substitution_has_execute_flag(program: &str) -> bool {
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
        } else if delimiters >= 2 && character == 'e' {
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
