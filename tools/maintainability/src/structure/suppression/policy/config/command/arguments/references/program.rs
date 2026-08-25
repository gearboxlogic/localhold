use super::{ValueSemantics, path};

pub(super) fn dynamic_loader_is_opaque(command: &str) -> bool {
    command == "ld.so" || command.starts_with("ld.so.") || command.starts_with("ld-") && command.contains(".so")
}

pub(super) fn bash_enable_loads_builtin(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == "-f" || argument.starts_with("-f") && argument.len() > 2)
}

pub(super) fn split_filter_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    dynamic_arguments_are_opaque(arguments, semantics)
        || arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
            let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
            option.len() >= "--f".len() && "--filter".starts_with(option)
        })
}

pub(super) fn tar_dispatch_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    if arguments
        .iter()
        .any(|argument| semantics.contains_dynamic(argument) && !isolated_tar_directory(argument, semantics))
    {
        return true;
    }
    let command_option = arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option
            .strip_prefix('-')
            .filter(|options| !options.starts_with('-'))
            .is_some_and(|options| options.contains(['F', 'I']))
            || [
                ("--checkpoint-action", "--checkpoint-a"),
                ("--info-script", "--info"),
                ("--new-volume-script", "--new-v"),
                ("--rmt-command", "--rmt"),
                ("--rsh-command", "--rsh"),
                ("--to-command", "--to-c"),
                ("--use-compress-program", "--use-c"),
            ]
            .iter()
            .any(|(full, minimum)| option.len() >= minimum.len() && full.starts_with(option))
    });
    command_option || tar_extracts_files(arguments) && !tar_extracts_into_isolated_directory(arguments, semantics)
}

fn tar_extracts_files(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").enumerate().any(|(index, argument)| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        let abbreviated_long_extract = option.len() >= "--ext".len() && "--extract".starts_with(option);
        let abbreviated_long_get = option.len() >= "--ge".len() && "--get".starts_with(option);
        let short_extract = option.strip_prefix('-').is_some_and(|flags| !flags.starts_with('-') && flags.contains('x'));
        let old_style_extract = index == 0 && option.chars().all(|character| character.is_ascii_alphabetic()) && option.contains('x');
        abbreviated_long_extract || abbreviated_long_get || short_extract || old_style_extract
    })
}

fn tar_extracts_into_isolated_directory(arguments: &[String], semantics: ValueSemantics) -> bool {
    if arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        matches!(option, "-P" | "--absolute-names" | "--transform" | "--xform")
    }) {
        return false;
    }

    let mut isolated = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        let destination = if matches!(argument.as_str(), "-C" | "--directory") {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument
                .strip_prefix("--directory=")
                .or_else(|| argument.strip_prefix("-C").filter(|value| !value.is_empty()))
        };
        if let Some(destination) = destination {
            if !isolated_tar_directory(destination, semantics) {
                return false;
            }
            isolated = true;
        }
        index += 1;
    }
    isolated
}

fn isolated_tar_directory(destination: &str, semantics: ValueSemantics) -> bool {
    if path::normalize_literal(destination).as_deref() == Some("extracted") {
        return true;
    }
    semantics == ValueSemantics::Shell
        && ["$RUNNER_TEMP/", "${RUNNER_TEMP}/"]
            .iter()
            .any(|prefix| destination.strip_prefix(prefix).is_some_and(|suffix| path::normalize_literal(suffix).is_some()))
}

pub(super) fn sort_compression_program_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    dynamic_arguments_are_opaque(arguments, semantics)
        || arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
            let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
            option.len() >= "--co".len() && "--compress-program".starts_with(option)
        })
}

pub(super) fn zip_test_command_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    dynamic_arguments_are_opaque(arguments, semantics)
        || arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
            let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
            [("--test", "--tes"), ("--unzip-command", "--unz")]
                .iter()
                .any(|(full, minimum)| option.len() >= minimum.len() && full.starts_with(option))
                || option
                    .strip_prefix('-')
                    .filter(|options| !options.starts_with('-'))
                    .is_some_and(|options| options.contains('T'))
        })
}

pub(super) fn openssl_module_selection_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    dynamic_arguments_are_opaque(arguments, semantics)
        || arguments.first().is_some_and(|argument| argument == "engine")
        || arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
            let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
            matches!(
                option,
                "-config"
                    | "--config"
                    | "-engine"
                    | "--engine"
                    | "-keygen_engine"
                    | "--keygen_engine"
                    | "-provider"
                    | "--provider"
                    | "-provider-path"
                    | "--provider-path"
                    | "-ssl_client_engine"
                    | "--ssl_client_engine"
            )
        })
}

pub(super) fn ripgrep_preprocessor_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    dynamic_arguments_are_opaque(arguments, semantics)
        || arguments
            .iter()
            .take_while(|argument| argument.as_str() != "--")
            .any(|argument| argument == "--pre" || argument.starts_with("--pre="))
}

pub(super) fn just_source_selection_with_semantics_is_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    dynamic_arguments_are_opaque(arguments, semantics)
        || arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
            matches!(
                argument.as_str(),
                "-c" | "-d"
                    | "-f"
                    | "-s"
                    | "--choose"
                    | "--chooser"
                    | "--command"
                    | "--dotenv-filename"
                    | "--dotenv-path"
                    | "--edit"
                    | "--global-justfile"
                    | "--init"
                    | "--justfile"
                    | "--search"
                    | "--shell"
                    | "--shell-arg"
                    | "--shell-command"
                    | "--working-directory"
            ) || [
                "--choose=",
                "--chooser=",
                "--command=",
                "--dotenv-filename=",
                "--dotenv-path=",
                "--justfile=",
                "--shell=",
                "--shell-arg=",
                "--shell-command=",
                "--working-directory=",
            ]
            .iter()
            .any(|prefix| argument.starts_with(prefix))
                || matches!(argument.as_bytes(), [b'-', b'c' | b'd' | b'f', ..] if argument.len() > 2)
        })
}

fn dynamic_arguments_are_opaque(arguments: &[String], semantics: ValueSemantics) -> bool {
    arguments.iter().any(|argument| semantics.contains_dynamic(argument))
}
