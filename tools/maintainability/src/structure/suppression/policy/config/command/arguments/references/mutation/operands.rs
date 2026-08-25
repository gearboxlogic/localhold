use super::{DispatchContext, path_policy};

pub(super) fn chmod_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let recursive = arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        argument
            .strip_prefix('-')
            .filter(|options| !options.starts_with('-'))
            .is_some_and(|options| options.contains('R'))
            || argument
                .split_once('=')
                .map_or(argument.as_str(), |(option, _)| option)
                .strip_prefix("--")
                .is_some_and(|option| option.len() >= "rec".len() && "recursive".starts_with(option))
    });
    recursive || chmod_targets_are_opaque(context, arguments)
}

fn chmod_targets_are_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut options_ended = false;
    let mut consumes_reference = false;
    let mut reference_mode = false;
    let mut mode_selected = false;
    for argument in arguments {
        if consumes_reference {
            consumes_reference = false;
            reference_mode = true;
            continue;
        }
        if !options_ended && argument == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && argument == "--reference" {
            consumes_reference = true;
            continue;
        }
        if !options_ended && argument.starts_with("--reference=") {
            reference_mode = true;
            continue;
        }
        if !options_ended && chmod_flag(argument) {
            continue;
        }
        if !reference_mode && !mode_selected {
            mode_selected = true;
            continue;
        }
        if path_policy::target_is_opaque(context.path_policy, argument, context.semantics) {
            return true;
        }
    }
    consumes_reference
}

fn chmod_flag(argument: &str) -> bool {
    argument
        .strip_prefix('-')
        .filter(|options| !options.is_empty() && !options.starts_with('-'))
        .is_some_and(|options| options.chars().all(|option| matches!(option, 'R' | 'c' | 'f' | 'v')))
        || matches!(
            argument,
            "--changes" | "--silent" | "--quiet" | "--verbose" | "--recursive" | "--preserve-root" | "--no-preserve-root"
        )
}

pub(super) fn sed_targets_are_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut options_ended = false;
    let mut consumes_program = false;
    let mut program_selected = false;
    for argument in arguments {
        if consumes_program {
            consumes_program = false;
            program_selected = true;
            continue;
        }
        if !options_ended && argument == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && matches!(argument.as_str(), "-e" | "--expression" | "-f" | "--file") {
            consumes_program = true;
            continue;
        }
        if !options_ended
            && (argument.strip_prefix("-e").is_some_and(|program| !program.is_empty())
                || argument.strip_prefix("-f").is_some_and(|program| !program.is_empty())
                || argument.starts_with("--expression=")
                || argument.starts_with("--file="))
        {
            program_selected = true;
            continue;
        }
        if !options_ended && argument.starts_with('-') {
            continue;
        }
        if !program_selected {
            program_selected = true;
            continue;
        }
        if path_policy::target_is_opaque(context.path_policy, argument, context.semantics) {
            return true;
        }
    }
    consumes_program
}

pub(super) fn perl_targets_are_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut options_ended = false;
    let mut consumes_program = false;
    let mut program_selected = false;
    for argument in arguments {
        if consumes_program {
            consumes_program = false;
            program_selected = true;
            continue;
        }
        if !options_ended && argument == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && argument.starts_with('-') {
            if let Some(position) = argument.char_indices().find_map(|(index, option)| matches!(option, 'e' | 'E').then_some(index)) {
                program_selected = true;
                consumes_program = argument[position + 1..].is_empty();
            }
            continue;
        }
        if !program_selected {
            program_selected = true;
            continue;
        }
        if path_policy::target_is_opaque(context.path_policy, argument, context.semantics) {
            return true;
        }
    }
    consumes_program
}

pub(super) fn sed_in_place_selected(arguments: &[String]) -> bool {
    short_option_before_operand(arguments, |option| match option {
        'i' => Some(true),
        'e' | 'f' => Some(false),
        _ => None,
    }) || arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option.strip_prefix("--").is_some_and(|option| !option.is_empty() && "in-place".starts_with(option))
    })
}

pub(super) fn perl_in_place_selected(arguments: &[String]) -> bool {
    short_option_before_operand(arguments, |option| match option {
        'i' => Some(true),
        '0' | 'C' | 'D' | 'e' | 'E' | 'F' | 'I' | 'M' | 'm' | 'x' => Some(false),
        _ => None,
    })
}

fn short_option_before_operand(arguments: &[String], classify: impl Fn(char) -> Option<bool>) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        argument
            .strip_prefix('-')
            .filter(|options| !options.starts_with('-'))
            .is_some_and(|options| options.chars().find_map(&classify).unwrap_or(false))
    })
}
