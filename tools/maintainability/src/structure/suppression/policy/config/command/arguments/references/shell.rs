use super::{SelectedInput, unknown};

pub(in crate::structure::suppression::policy::config::command::arguments) fn is_additional(command: &str) -> bool {
    matches!(
        command,
        "ash"
            | "ash.exe"
            | "hush"
            | "hush.exe"
            | "ksh"
            | "ksh.exe"
            | "mksh"
            | "mksh.exe"
            | "oksh"
            | "oksh.exe"
            | "osh"
            | "osh.exe"
            | "pdksh"
            | "pdksh.exe"
            | "posh"
            | "posh.exe"
            | "yash"
            | "yash.exe"
    )
}

pub(super) fn input<'a>(command: &str, arguments: &'a [String]) -> SelectedInput<'a> {
    if matches!(command, "fish" | "fish.exe" | "zsh" | "zsh.exe") {
        return SelectedInput::Opaque;
    }
    for (index, argument) in arguments.iter().enumerate() {
        let lowercase = argument.to_ascii_lowercase();
        if matches!(lowercase.as_str(), "--help" | "--version") {
            return SelectedInput::None;
        }
        if matches!(lowercase.as_str(), "-c" | "--command" | "-i" | "--interactive" | "-l" | "--login" | "-s" | "--stdin")
            || (argument.starts_with('-') || argument.starts_with('+')) && !argument.starts_with("--") && argument[1..].contains(['c', 'C', 'i', 'l', 's', 'o', 'O'])
            || matches!(lowercase.as_str(), "--init-file" | "--rcfile")
            || lowercase.starts_with("--init-file=")
            || lowercase.starts_with("--rcfile=")
        {
            return SelectedInput::Opaque;
        }
        if argument.starts_with("<<") {
            return SelectedInput::Opaque;
        }
        if argument == "--" {
            return arguments.get(index + 1).map_or(SelectedInput::Opaque, |candidate| script(candidate));
        }
        if argument.starts_with(['-', '+']) {
            continue;
        }
        return script(argument);
    }
    SelectedInput::Opaque
}

pub(super) fn source_input(arguments: &[String]) -> SelectedInput<'_> {
    match arguments {
        [path, ..] if !path.starts_with('-') => script(path),
        [option, path, ..] if option == "--" => script(path),
        _ => SelectedInput::Opaque,
    }
}

fn script(candidate: &str) -> SelectedInput<'_> {
    if unknown::is_rust_source(candidate) {
        SelectedInput::Opaque
    } else {
        SelectedInput::Literal(candidate)
    }
}
