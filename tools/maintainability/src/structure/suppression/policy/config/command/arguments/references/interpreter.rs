use std::path::Path;

use super::SelectedInput;

pub(super) fn python_input(arguments: &[String]) -> SelectedInput<'_> {
    let mut after_options = false;
    for argument in arguments {
        if !after_options && matches!(argument.as_str(), "-h" | "--help" | "-V" | "--version") {
            return SelectedInput::None;
        }
        if !after_options && matches!(argument.as_str(), "-c" | "-m") {
            return SelectedInput::Opaque;
        }
        if !after_options && argument == "--" {
            after_options = true;
            continue;
        }
        if !after_options && argument.starts_with('-') {
            if python_flag_without_operand(argument) {
                continue;
            }
            return SelectedInput::Opaque;
        }
        return if argument == "-" {
            SelectedInput::Opaque
        } else if Path::new(argument)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
        {
            SelectedInput::Literal(argument)
        } else {
            SelectedInput::Opaque
        };
    }
    SelectedInput::Opaque
}

fn python_flag_without_operand(argument: &str) -> bool {
    matches!(
        argument,
        "-b" | "-bb" | "-B" | "-d" | "-E" | "-I" | "-O" | "-OO" | "-P" | "-q" | "-R" | "-s" | "-S" | "-u" | "-v" | "-x"
    ) || argument.starts_with("-W") && argument.len() > 2
        || argument.starts_with("-X") && argument.len() > 2
        || argument.starts_with("--check-hash-based-pycs=")
}

pub(super) fn powershell_input(arguments: &[String]) -> SelectedInput<'_> {
    for (index, argument) in arguments.iter().enumerate() {
        let lowercase = argument.to_ascii_lowercase();
        if matches!(lowercase.as_str(), "-help" | "-h" | "-?" | "-version") {
            return SelectedInput::None;
        }
        if matches!(lowercase.as_str(), "-command" | "-c" | "-encodedcommand" | "-ec") {
            return SelectedInput::Opaque;
        }
        if matches!(lowercase.as_str(), "-file" | "-f") {
            return arguments.get(index + 1).map_or(SelectedInput::Opaque, |candidate| SelectedInput::Literal(candidate));
        }
        if argument.starts_with('-') {
            if powershell_flag_without_operand(&lowercase) {
                continue;
            }
            return SelectedInput::Opaque;
        }
        return SelectedInput::Literal(argument);
    }
    SelectedInput::Opaque
}

fn powershell_flag_without_operand(argument: &str) -> bool {
    matches!(
        argument,
        "-login" | "-mta" | "-noexit" | "-nologo" | "-noninteractive" | "-noprofile" | "-noprofileloadtime" | "-sshservermode" | "-sta"
    )
}
