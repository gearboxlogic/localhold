use std::collections::BTreeSet;

use super::{is_environment_assignment, is_shell_command_prefix, tokens};

mod arithmetic;

pub(super) fn assigned_variables(source: &str) -> (BTreeSet<String>, bool) {
    let mut names = BTreeSet::new();
    let mut integer_names = BTreeSet::new();
    let mut opaque_target = false;
    for command in tokens::source_command_tokens(source) {
        let Some(index) = command_word_index(&command) else {
            continue;
        };
        let arguments = &command[index.saturating_add(1)..];
        let command_word = command[index].trim_matches(['(', ')', '{', '}']).to_ascii_lowercase();
        match command_word.as_str() {
            "declare" | "local" | "typeset" => {
                opaque_target |= declaration_has_opaque_target(arguments, true, true);
                update_integer_attributes(arguments, &mut integer_names);
            }
            "readonly" => {
                opaque_target |= declaration_has_opaque_target(arguments, false, true);
                update_integer_attributes(arguments, &mut integer_names);
            }
            "export" if declaration_has_opaque_target(arguments, false, false) => opaque_target = true,
            "printf" | "read" | "readarray" | "mapfile" | "getopts" => {
                let mut assigned = BTreeSet::new();
                match command_word.as_str() {
                    "printf" => collect_printf_target(arguments, &mut assigned, &mut opaque_target),
                    "read" | "readarray" | "mapfile" => collect_read_targets(&command_word, arguments, &mut assigned, &mut opaque_target),
                    "getopts" => collect_getopts_target(arguments, &mut assigned, &mut opaque_target),
                    _ => unreachable!("matched assignment builtin"),
                }
                opaque_target |= assigned.iter().any(|name| integer_names.contains(name));
                names.extend(assigned);
            }
            _ => {}
        }
    }
    (names, opaque_target)
}

fn update_integer_attributes(arguments: &[String], integer_names: &mut BTreeSet<String>) {
    let mut options = true;
    let mut integer_attribute = None;
    for argument in arguments {
        let word = argument.trim_matches(['\'', '"', ';']);
        if options && word == "--" {
            options = false;
            continue;
        }
        if options && word.len() > 1 && matches!(word.as_bytes()[0], b'-' | b'+') {
            if word[1..].contains('i') {
                integer_attribute = Some(word.starts_with('-'));
            }
            continue;
        }
        let target = word.split_once('=').map_or(word, |(target, _)| target);
        let target = target.strip_suffix('+').unwrap_or(target);
        let Some(name) = identifier(target) else {
            continue;
        };
        match integer_attribute {
            Some(true) => {
                integer_names.insert(name.to_owned());
            }
            Some(false) => {
                integer_names.remove(name);
            }
            None => {}
        }
    }
}

pub(super) fn has_opaque_indexed_assignment(path: &str, source: &str, source_is_reviewed: bool) -> bool {
    tokens::source_command_tokens(source)
        .iter()
        .flatten()
        .any(|word| indexed_assignment_is_opaque(path, word, source_is_reviewed))
}

pub(super) fn has_opaque_arithmetic_evaluation(path: &str, source: &str, source_is_reviewed: bool) -> bool {
    arithmetic::has_opaque_evaluation(path, source, source_is_reviewed)
}

pub(super) fn has_opaque_unset_target(path: &str, source: &str, source_is_reviewed: bool) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| {
        let Some(index) = command_word_index(command) else {
            return false;
        };
        let command_word = command[index].trim_matches(['(', ')', '{', '}']);
        command_word.eq_ignore_ascii_case("unset")
            && command[index + 1..].iter().any(|argument| {
                let target = argument.trim_matches(['\'', '"', ';']);
                !target.starts_with('-') && target != "--" && identifier(target).is_none() && !reviewed_environment_scrubber_unset(path, source, source_is_reviewed, target)
            })
    })
}

fn declaration_has_opaque_target(arguments: &[String], reject_nameref: bool, supports_integer_attribute: bool) -> bool {
    let mut options = true;
    let mut integer_attribute = false;
    arguments.iter().any(|argument| {
        let word = argument.trim_matches(['\'', '"', ';']);
        if options && word == "--" {
            options = false;
            return false;
        }
        if options && word.len() > 1 && matches!(word.as_bytes()[0], b'-' | b'+') {
            if supports_integer_attribute && word[1..].contains('i') {
                integer_attribute = word.starts_with('-');
            }
            return reject_nameref && word[1..].contains('n');
        }
        let (target, value) = word.split_once('=').map_or((word, None), |(target, value)| (target, Some(value)));
        if value.is_some_and(executable_declaration_value) || integer_attribute && value.is_some_and(integer_value_is_opaque) {
            return true;
        }
        let target = target.strip_suffix('+').unwrap_or(target);
        identifier(target).is_none()
    })
}

fn executable_declaration_value(value: &str) -> bool {
    value.contains("$(") || value.contains('`') || value.starts_with('(') && value != "()"
}

fn integer_value_is_opaque(value: &str) -> bool {
    let literal = value.strip_prefix(['+', '-']).unwrap_or(value);
    !literal.is_empty() && !literal.bytes().all(|byte| byte.is_ascii_digit())
}

fn indexed_assignment_is_opaque(path: &str, word: &str, source_is_reviewed: bool) -> bool {
    let word = word.trim_matches(['(', ')', '{', '}', '\'', '"', ';']);
    let Some((target, _)) = word.split_once('=') else {
        return false;
    };
    let target = target.strip_suffix('+').unwrap_or(target);
    let Some((name, subscript)) = target.split_once('[') else {
        return false;
    };
    if identifier(name).is_none() {
        return false;
    }
    let Some(subscript) = subscript.strip_suffix(']') else {
        return true;
    };
    subscript.is_empty() || !(arithmetic::literal_subscript(subscript) || arithmetic::reviewed_associative_subscript(path, source_is_reviewed, name, subscript))
}

fn reviewed_environment_scrubber_unset(path: &str, source: &str, source_is_reviewed: bool, target: &str) -> bool {
    source_is_reviewed
        && path == "script/check-maintainability-bootstrap.sh"
        && target == "$name"
        && source.contains("while IFS= read -r name; do")
        && source.contains("uppercase=${name^^}\n        case \"$uppercase\" in")
        && source.contains("unset \"$name\"\n                ;;\n        esac\n    done < <(compgen -e)")
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
    let mut index = 0;
    let mut options = true;
    while index < arguments.len() {
        let argument = arguments[index].trim_matches(['\'', '"', ';']);
        if let Some(has_attached_operand) = redirection_has_attached_operand(argument) {
            if !has_attached_operand && arguments.get(index + 1).is_none() {
                *opaque_target = true;
            }
            if let Some(next) = index_after_redirection(arguments, index, has_attached_operand) {
                index = next;
            } else {
                *opaque_target = true;
                break;
            }
            continue;
        }
        if options && argument == "--" {
            options = false;
            index += 1;
            continue;
        }
        if options && argument.starts_with('-') && argument != "-" {
            let mut state = ReadTargetState {
                names,
                found: &mut found,
                opaque: opaque_target,
            };
            index = collect_read_option(command, arguments, index, &mut state);
            continue;
        }
        options = false;
        if identifier(argument).is_some() {
            collect_target(Some(argument), names, opaque_target);
            found = true;
        } else if argument.contains(['[', ']']) || contains_dynamic_target(argument) {
            *opaque_target = true;
        }
        index += 1;
    }
    if !found {
        names.insert(default.to_owned());
    }
}

fn redirection_has_attached_operand(word: &str) -> Option<bool> {
    let word = word.trim_start_matches(|character: char| character.is_ascii_digit());
    let operand = ["<<<", "<<-", "<<", "<&", ">&", "<>", ">>", ">|", "<", ">"]
        .into_iter()
        .find_map(|operator| word.strip_prefix(operator))
        .or_else(|| ["&>>", "&>"].into_iter().find_map(|operator| word.strip_prefix(operator)))?;
    Some(!operand.is_empty())
}

fn index_after_redirection(arguments: &[String], index: usize, has_attached_operand: bool) -> Option<usize> {
    if has_attached_operand {
        return Some(index + 1);
    }
    let Some(operand) = arguments.get(index + 1) else {
        return Some(index + 1);
    };
    let mut next = index + 2;
    if !(operand.starts_with("<(") || operand.starts_with(">(")) {
        return Some(next);
    }
    let mut depth = parenthesis_delta(operand);
    while depth > 0
        && let Some(word) = arguments.get(next)
    {
        depth += parenthesis_delta(word);
        next += 1;
    }
    (depth == 0).then_some(next)
}

fn parenthesis_delta(word: &str) -> isize {
    word.bytes().fold(0, |depth, byte| match byte {
        b'(' => depth + 1,
        b')' => depth - 1,
        _ => depth,
    })
}

struct ReadTargetState<'a> {
    names: &'a mut BTreeSet<String>,
    found: &'a mut bool,
    opaque: &'a mut bool,
}

fn collect_read_option(command: &str, arguments: &[String], index: usize, state: &mut ReadTargetState<'_>) -> usize {
    let argument = arguments[index].trim_matches(['\'', '"', ';']);
    let Ok(option) = option_operand(command, argument) else {
        *state.opaque = true;
        return index + 1;
    };
    let Some(option) = option else {
        return index + 1;
    };
    let following = arguments.get(index + 1).map(String::as_str);
    match option.operand {
        OptionOperand::Callback => *state.opaque = true,
        OptionOperand::Target => {
            collect_target(option.attached.or(following), state.names, state.opaque);
            *state.found = true;
        }
        OptionOperand::Value => *state.opaque |= option.attached.is_none() && following.is_none(),
    }
    index + 1 + usize::from(option.attached.is_none())
}

#[derive(Clone, Copy)]
enum OptionOperand {
    Callback,
    Target,
    Value,
}

struct ParsedOption<'a> {
    operand: OptionOperand,
    attached: Option<&'a str>,
}

fn option_operand<'a>(command: &str, word: &'a str) -> Result<Option<ParsedOption<'a>>, ()> {
    let options = word.strip_prefix('-').filter(|options| !options.is_empty()).ok_or(())?;
    for (offset, option) in options.char_indices() {
        let operand = match (command, option) {
            ("read", 'a') => Some(OptionOperand::Target),
            ("mapfile" | "readarray", 'C') => Some(OptionOperand::Callback),
            ("read", 'd' | 'i' | 'n' | 'N' | 'p' | 't' | 'u') | ("mapfile" | "readarray", 'c' | 'd' | 'n' | 'O' | 's' | 'u') => Some(OptionOperand::Value),
            ("read", 'e' | 'r' | 's') | ("mapfile" | "readarray", 't') => None,
            _ => return Err(()),
        };
        if let Some(operand) = operand {
            let attached = &options[offset + option.len_utf8()..];
            return Ok(Some(ParsedOption {
                operand,
                attached: (!attached.is_empty()).then_some(attached),
            }));
        }
    }
    Ok(None)
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
    is_identifier(word).then_some(word)
}

fn is_identifier(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !word.as_bytes()[0].is_ascii_digit()
}

fn contains_dynamic_target(word: &str) -> bool {
    word.contains(['$', '%', '!'])
}

#[cfg(test)]
mod tests {
    use super::{assigned_variables, has_opaque_indexed_assignment, has_opaque_unset_target};

    #[test]
    fn mutating_builtins_expose_their_assignment_targets() {
        let (names, opaque) = assigned_variables("printf -v tool %b payload; IFS= read -r sub option; mapfile -t commands; getopts ab flag");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["commands", "flag", "option", "sub", "tool"]);
        assert!(!opaque);

        let (names, opaque) = assigned_variables("{read < input; (mapfile) < input");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["MAPFILE", "REPLY"]);
        assert!(!opaque);

        let (names, opaque) = assigned_variables("printf -v \"$target\" %s cargo");
        assert!(names.is_empty());
        assert!(opaque);

        let (_, opaque) = assigned_variables("local -n command_alias=container_cli");
        assert!(opaque);
    }

    #[test]
    fn declarations_and_unset_fail_closed_around_dynamic_evaluation() {
        for source in [
            "local 'a[$(bash quality/hidden.txt)]=x'",
            "declare -a 'a[$(bash quality/hidden.txt)]=x'",
            "typeset -- 'a[`bash quality/hidden.txt`]+=x'",
            "local -a 'a=([$(bash quality/hidden.txt)]=x)'",
            "local -i 'x=a[$(bash quality/hidden.txt)]'",
            "readonly -a 'a=([$(bash quality/hidden.txt)]=x)'",
            "export -a 'a=([$(bash quality/hidden.txt)]=x)'",
            "declare -a 'a=([`bash quality/hidden.txt`]=x)'",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(opaque, "{source}");
        }

        assert!(has_opaque_unset_target("script/check.sh", "unset 'a[payload]'", false));
        assert!(has_opaque_unset_target("script/check.sh", "unset \"$name\"", true));
        let scrubber = concat!(
            "while IFS= read -r name; do\n",
            "        uppercase=${name^^}\n        case \"$uppercase\" in\n",
            "            SAFE)\n                unset \"$name\"\n                ;;\n        esac\n    done < <(compgen -e)\n"
        );
        assert!(!has_opaque_unset_target("script/check-maintainability-bootstrap.sh", scrubber, true));
        assert!(has_opaque_unset_target("script/check-maintainability-bootstrap.sh", scrubber, false));
        assert!(!has_opaque_unset_target("script/check.sh", "unset -f function_name", false));

        for source in [
            "local value=x",
            "declare -a values",
            "typeset -- value+=x",
            "local -A expected_paths=()",
            "local -i count=0",
            "declare -i offset=-12",
            "readonly value=x",
            "export -n PATH",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(!opaque, "{source}");
        }

        for source in [
            "local -i count=$payload",
            "declare -ai values=payload",
            "typeset -i offset='${payload}'",
            "readonly -i count=payload",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(opaque, "{source}");
        }
    }

    #[test]
    fn integer_attributes_make_later_builtin_assignments_opaque() {
        for source in [
            "declare -i value=0; read -r value",
            "local -i value; printf -v value %s payload",
            "typeset -ai values; mapfile -t values",
            "readonly -i REPLY=0; read",
            "declare -ai MAPFILE; readarray",
            "declare -i option=0; getopts ab option",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(opaque, "{source}");
        }
        for source in [
            "declare value=0; read -r value",
            "declare -i value=0; declare +i value; printf -v value %s payload",
            "declare -i count=0; read -r value",
            "printf -v value %s payload; declare -i value=0",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(!opaque, "{source}");
        }
    }

    #[test]
    fn read_family_targets_survive_options_and_redirections() {
        for source in [
            "read 'a[payload]'",
            "read -a 'a[payload]'",
            "read -r -d '' 'a[payload]'",
            "mapfile 'a[payload]'",
            "readarray -t 'a[payload]'",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(opaque, "{source}");
        }

        for source in ["read -r -d '' entry", "read -r mode object_type expected_hash", "read -r -a path_directories"] {
            let (_, opaque) = assigned_variables(source);
            assert!(!opaque, "{source}");
        }

        for source in [
            "read -p '[yes/no] ' answer",
            "read -d ']' answer",
            "read -d] answer",
            "mapfile -d ']' lines",
            "readarray -d] lines",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(!opaque, "{source}");
        }

        for source in ["mapfile -C 'bash quality/hidden.txt' -c 1 lines", "readarray -C'bash quality/hidden.txt' -c1 lines"] {
            let (_, opaque) = assigned_variables(source);
            assert!(opaque, "{source}");
        }

        for source in [
            "read < input 'a[payload]'",
            "read 3< input 'a[payload]'",
            "read <input 'a[payload]'",
            "mapfile > output 'a[payload]'",
            "readarray 3<& 0 'a[payload]'",
        ] {
            let (_, opaque) = assigned_variables(source);
            assert!(opaque, "{source}");
        }
        for source in ["read < input answer", "mapfile 3<input lines", "readarray > output values"] {
            let (_, opaque) = assigned_variables(source);
            assert!(!opaque, "{source}");
        }
        let (names, opaque) = assigned_variables("read -r first < <(printf '%s\\n' value) second");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["first", "second"]);
        assert!(!opaque);
    }

    #[test]
    fn indexed_assignments_require_literal_or_profiled_targets() {
        assert!(has_opaque_indexed_assignment("script/check.sh", "a['$(bash quality/hidden.txt)']=x", false));
        assert!(has_opaque_indexed_assignment("script/check.sh", "a[payload]+=x", false));
        assert!(!has_opaque_indexed_assignment("script/check.sh", "a[0]=x", false));
        assert!(has_opaque_indexed_assignment("script/check.sh", "a[$payload]=x", true));
        assert!(!has_opaque_indexed_assignment(
            "script/check-maintainability-bootstrap.sh",
            "expected_paths[$relative_path]=1",
            true
        ));
        assert!(has_opaque_indexed_assignment("script/check-maintainability-bootstrap.sh", "other[$relative_path]=1", true));
        assert!(has_opaque_indexed_assignment("script/check.sh", "a[$(bash)]=x", true));
    }
}
