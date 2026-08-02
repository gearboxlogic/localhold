use super::{destination_is_opaque, path};

pub(super) fn dispatch_is_opaque(path: &str, command: &str, arguments: &[String]) -> bool {
    if !is_strip_command(command) {
        return false;
    }
    let mut index = 0;
    let mut output_selected = false;
    let mut inputs = Vec::new();
    while let Some(argument) = arguments.get(index) {
        if matches!(argument.as_str(), "-h" | "--help" | "--info" | "-V" | "--version") {
            return false;
        }
        if argument == "--" {
            inputs.extend(arguments[index + 1..].iter().map(String::as_str));
            break;
        }
        if let Some(output) = output_target(argument, arguments, &mut index) {
            output_selected = true;
            if destination_is_opaque(path, output) {
                return true;
            }
        } else if option_consumes_next(argument) {
            index += 1;
            if arguments.get(index).is_none() {
                return true;
            }
        } else if !argument.starts_with('-') {
            inputs.push(argument);
        }
        index += 1;
    }
    !output_selected && inputs.into_iter().any(in_place_target_is_opaque)
}

fn is_strip_command(command: &str) -> bool {
    let stem = command.strip_suffix(".exe").unwrap_or(command);
    let unversioned = stem.trim_end_matches(|character: char| character.is_ascii_digit() || matches!(character, '.' | '-'));
    unversioned == "strip" || unversioned.ends_with("-strip")
}

fn output_target<'a>(argument: &'a str, arguments: &'a [String], index: &mut usize) -> Option<&'a str> {
    if matches!(argument, "-o" | "--output" | "--output-file") {
        *index += 1;
        return Some(arguments.get(*index).map_or("", String::as_str));
    }
    argument
        .strip_prefix("-o")
        .filter(|target| !target.is_empty())
        .or_else(|| argument.strip_prefix("--output=").or_else(|| argument.strip_prefix("--output-file=")))
}

fn option_consumes_next(argument: &str) -> bool {
    matches!(
        argument,
        "-F" | "--input-target"
            | "-I"
            | "--keep-section"
            | "--keep-symbol"
            | "-K"
            | "-N"
            | "-O"
            | "--output-target"
            | "--plugin"
            | "-R"
            | "--remove-relocations"
            | "--remove-section"
            | "--strip-symbol"
            | "--target"
    )
}

fn in_place_target_is_opaque(target: &str) -> bool {
    if let Some(target) = path::normalize_literal(target) {
        return super::super::super::super::is_protected_check_input(&target);
    }
    path::contains_dynamic_value(target) || !path::is_absolute(target)
}
