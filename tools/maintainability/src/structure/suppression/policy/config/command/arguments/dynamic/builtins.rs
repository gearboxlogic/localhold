use std::collections::BTreeSet;

use super::{is_environment_assignment, is_shell_command_prefix, tokens};

pub(super) fn assigned_variables(source: &str) -> (BTreeSet<String>, bool) {
    let mut names = BTreeSet::new();
    let mut opaque_target = false;
    for command in tokens::source_command_tokens(source) {
        let Some(index) = command_word_index(&command) else {
            continue;
        };
        let arguments = &command[index.saturating_add(1)..];
        match command[index].trim_matches(['(', ')', '{', '}']).to_ascii_lowercase().as_str() {
            "printf" => collect_printf_target(arguments, &mut names, &mut opaque_target),
            "read" | "readarray" | "mapfile" => collect_read_targets(&command[index], arguments, &mut names, &mut opaque_target),
            "getopts" => collect_getopts_target(arguments, &mut names, &mut opaque_target),
            _ => {}
        }
    }
    (names, opaque_target)
}

fn command_word_index(command: &[String]) -> Option<usize> {
    command.iter().position(|word| {
        let word = word.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word)
    })
}

fn collect_printf_target(arguments: &[String], names: &mut BTreeSet<String>, opaque_target: &mut bool) {
    let target = arguments
        .windows(2)
        .find_map(|pair| (pair[0] == "-v").then_some(pair[1].as_str()))
        .or_else(|| arguments.iter().find_map(|argument| argument.strip_prefix("-v").filter(|target| !target.is_empty())));
    if let Some(target) = target {
        collect_target(Some(target), names, opaque_target);
    }
}

fn collect_read_targets(command: &str, arguments: &[String], names: &mut BTreeSet<String>, opaque_target: &mut bool) {
    let default = if command.eq_ignore_ascii_case("read") { "REPLY" } else { "MAPFILE" };
    let mut found = false;
    for argument in arguments {
        if argument.starts_with(['<', '>']) {
            break;
        }
        if argument.starts_with('-') {
            continue;
        }
        if identifier(argument).is_some() {
            collect_target(Some(argument.as_str()), names, opaque_target);
            found = true;
        } else if contains_dynamic_target(argument) {
            *opaque_target = true;
        }
    }
    if !found {
        names.insert(default.to_owned());
    }
}

fn collect_getopts_target(arguments: &[String], names: &mut BTreeSet<String>, opaque_target: &mut bool) {
    collect_target(arguments.get(1).map(String::as_str), names, opaque_target);
}

fn collect_target(target: Option<&str>, names: &mut BTreeSet<String>, opaque_target: &mut bool) {
    let Some(target) = target else {
        *opaque_target = true;
        return;
    };
    if let Some(name) = identifier(target) {
        names.insert(name.to_owned());
    } else {
        *opaque_target = true;
    }
}

fn identifier(word: &str) -> Option<&str> {
    let word = word.trim_matches(['\'', '"', ';']);
    (!word.is_empty() && word.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !word.as_bytes()[0].is_ascii_digit()).then_some(word)
}

fn contains_dynamic_target(word: &str) -> bool {
    word.contains(['$', '%', '!'])
}

#[cfg(test)]
mod tests {
    use super::assigned_variables;

    #[test]
    fn mutating_builtins_expose_their_assignment_targets() {
        let (names, opaque) = assigned_variables("printf -v tool %b payload; IFS= read -r sub option; mapfile -t commands; getopts ab flag");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["commands", "flag", "option", "sub", "tool"]);
        assert!(!opaque);

        let (names, opaque) = assigned_variables("printf -v \"$target\" %s cargo");
        assert!(names.is_empty());
        assert!(opaque);
    }
}
