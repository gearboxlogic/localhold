use super::{destination_is_opaque, is_objcopy_command, short_output_target};

pub(super) fn dispatch_is_opaque(path: &str, command: &str, arguments: &[String]) -> bool {
    match command {
        "find" | "find.exe" => find_output_is_opaque(path, arguments),
        "git" | "git.exe" => long_output_is_opaque(path, arguments, "--output", "--out"),
        "sort" | "sort.exe" => sort_output_is_opaque(path, arguments),
        _ if is_objcopy_command(command) => objcopy_section_output_is_opaque(path, arguments),
        _ if super::super::compiler::accepts_output_path(command) => compiler_output_is_opaque(path, arguments),
        _ => false,
    }
}

fn find_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if matches!(argument.as_str(), "-fls" | "-fprint" | "-fprint0" | "-fprintf") {
            index += 1;
            if destination_is_opaque(path, arguments.get(index).map_or("", String::as_str)) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn objcopy_section_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let (option, attached) = argument.split_once('=').map_or((argument.as_str(), None), |(option, value)| (option, Some(value)));
        if option.len() >= "--dump-s".len() && "--dump-section".starts_with(option) {
            let specification = attached.unwrap_or_else(|| {
                index += 1;
                arguments.get(index).map_or("", String::as_str)
            });
            let destination = specification.split_once('=').map_or("", |(_, destination)| destination);
            if destination_is_opaque(path, destination) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn sort_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let destination = long_output_target(argument, arguments, &mut index, "--output", "--out").or_else(|| short_output_target(argument, arguments, &mut index));
        if destination.is_some_and(|destination| destination_is_opaque(path, destination)) {
            return true;
        }
        index += 1;
    }
    false
}

fn compiler_output_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let destination = long_output_target(argument, arguments, &mut index, "--output", "--output").or_else(|| {
            if argument == "-o" {
                index += 1;
                arguments.get(index).map(String::as_str)
            } else {
                argument.strip_prefix("-o").filter(|destination| !destination.is_empty())
            }
        });
        if destination.is_some_and(|destination| destination_is_opaque(path, destination)) {
            return true;
        }
        index += 1;
    }
    false
}

fn long_output_is_opaque(path: &str, arguments: &[String], full: &str, minimum: &str) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if long_output_target(argument, arguments, &mut index, full, minimum).is_some_and(|destination| destination_is_opaque(path, destination)) {
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
