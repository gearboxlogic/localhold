use super::{DispatchContext, destination_is_opaque, is_objcopy_command, short_output_target};

pub(super) fn dispatch_is_opaque(context: DispatchContext<'_>, command: &str, arguments: &[String]) -> bool {
    match command {
        "find" | "find.exe" => find_output_is_opaque(context, arguments),
        "git" | "git.exe" => long_output_is_opaque(context, arguments, "--output", "--out"),
        "sort" | "sort.exe" => sort_output_is_opaque(context, arguments),
        "sponge" | "sponge.exe" => sponge_output_is_opaque(context, arguments),
        _ if is_objcopy_command(command) => objcopy_section_output_is_opaque(context, arguments),
        _ if super::super::compiler::accepts_output_path(command) => compiler_output_is_opaque(context, command, arguments),
        _ => false,
    }
}

fn sponge_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut accepts_options = true;
    let mut destination = None;
    for argument in arguments {
        if accepts_options && argument == "--" {
            accepts_options = false;
        } else if accepts_options && matches!(argument.as_str(), "-a" | "--append") {
        } else if accepts_options && matches!(argument.as_str(), "--help" | "--version") {
            return false;
        } else if accepts_options && argument.starts_with('-') || destination.replace(argument.as_str()).is_some() {
            return true;
        }
    }
    destination.is_some_and(|destination| destination_is_opaque(context, destination))
}

fn find_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if matches!(argument.as_str(), "-fls" | "-fprint" | "-fprint0" | "-fprintf") {
            index += 1;
            if destination_is_opaque(context, arguments.get(index).map_or("", String::as_str)) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn objcopy_section_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let (option, attached) = argument.split_once('=').map_or((argument.as_str(), None), |(option, value)| (option, Some(value)));
        if option.len() >= "--dump-s".len() && "--dump-section".starts_with(option) {
            let specification = attached.unwrap_or_else(|| {
                index += 1;
                arguments.get(index).map_or("", String::as_str)
            });
            let destination = specification.split_once('=').map_or("", |(_, destination)| destination);
            if destination_is_opaque(context, destination) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn sort_output_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let destination = long_output_target(argument, arguments, &mut index, "--output", "--out").or_else(|| short_output_target(argument, arguments, &mut index));
        if destination.is_some_and(|destination| destination_is_opaque(context, destination)) {
            return true;
        }
        index += 1;
    }
    false
}

fn compiler_output_is_opaque(context: DispatchContext<'_>, command: &str, arguments: &[String]) -> bool {
    let accepts_dependencies = super::super::compiler::is_compiler_driver(command);
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let destination = long_output_target(argument, arguments, &mut index, "--output", "--output").or_else(|| {
            let option = if accepts_dependencies && argument.starts_with("-MF") { "-MF" } else { "-o" };
            if argument == option {
                index += 1;
                return arguments.get(index).map(String::as_str);
            }
            argument.strip_prefix(option).filter(|destination| !destination.is_empty())
        });
        if destination.is_some_and(|destination| destination_is_opaque(context, destination)) {
            return true;
        }
        index += 1;
    }
    false
}

fn long_output_is_opaque(context: DispatchContext<'_>, arguments: &[String], full: &str, minimum: &str) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if long_output_target(argument, arguments, &mut index, full, minimum).is_some_and(|destination| destination_is_opaque(context, destination)) {
            return true;
        }
        index += 1;
    }
    false
}

fn long_output_target<'a>(argument: &'a str, arguments: &'a [String], index: &mut usize, full: &str, minimum: &str) -> Option<&'a str> {
    let (option, attached) = argument.split_once('=').map_or((argument, None), |(option, destination)| (option, Some(destination)));
    if option.len() < minimum.len() || !full.starts_with(option) {
        return None;
    }
    Some(attached.unwrap_or_else(|| {
        *index += 1;
        arguments.get(*index).map_or("", String::as_str)
    }))
}
