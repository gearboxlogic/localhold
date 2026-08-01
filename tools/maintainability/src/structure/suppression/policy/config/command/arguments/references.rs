use std::collections::BTreeSet;
use std::path::Path;

use super::{
    dynamic_program, is_cargo_tool_token, is_environment_assignment, is_normalized_manifest_path, is_yaml, matches_tool_name, normalized_source_for_surface, package_json, tokens,
    tool_basename,
};

mod cargo;
mod compiler;
mod git;
mod mutation;
mod native;
mod path;
mod sed;
mod trap;
pub(super) mod wrapper;

pub(in crate::structure::suppression::policy::config::command) fn cargo_manifest_paths_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = true;
    if let Some(scripts) = package_json::script_commands(path, source) {
        let Ok(scripts) = scripts else {
            return (BTreeSet::new(), true);
        };
        return collect_cargo_manifest_paths(scripts.iter().map(String::as_str), case_insensitive_tools);
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return collect_cargo_manifest_paths(embedded_commands.iter().map(String::as_str), case_insensitive_tools);
    }
    collect_cargo_manifest_paths(std::iter::once(source.as_str()).chain(embedded_commands.iter().map(String::as_str)), case_insensitive_tools)
}

pub(in crate::structure::suppression::policy::config::command) fn execution_inputs_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    if let Some(scripts) = package_json::script_commands(path, source) {
        return scripts.map_or_else(
            |_| (BTreeSet::new(), true),
            |scripts| collect_execution_inputs(scripts.iter().map(String::as_str), true, path),
        );
    }
    let source = normalized_source_for_surface(path, source);
    let embedded_commands = super::super::yaml::run_commands(path, &source);
    if is_yaml(path) {
        return collect_execution_inputs(embedded_commands.iter().map(String::as_str), true, path);
    }
    collect_execution_inputs(
        std::iter::once(source.as_str()).chain(embedded_commands.iter().map(String::as_str)),
        supports_direct_program_paths(path),
        path,
    )
}

fn collect_cargo_manifest_paths<'a>(sources: impl IntoIterator<Item = &'a str>, case_insensitive_tools: bool) -> (BTreeSet<String>, bool) {
    let mut paths = BTreeSet::new();
    let mut unresolved = false;
    for source in sources {
        collect_cargo_manifest_paths_from_source(source, case_insensitive_tools, &mut paths, &mut unresolved);
    }
    (paths, unresolved)
}

fn collect_cargo_manifest_paths_from_source(source: &str, case_insensitive_tools: bool, paths: &mut BTreeSet<String>, unresolved: &mut bool) {
    collect_cargo_manifest_paths_from_source_at_depth(source, case_insensitive_tools, paths, unresolved, 0);
}

fn collect_cargo_manifest_paths_from_source_at_depth(source: &str, case_insensitive_tools: bool, paths: &mut BTreeSet<String>, unresolved: &mut bool, depth: usize) {
    const MAX_NESTED_COMMAND_DEPTH: usize = 32;
    if depth == MAX_NESTED_COMMAND_DEPTH {
        *unresolved = true;
        return;
    }
    for tokens in tokens::normalized_shell_commands(source) {
        for nested in tokens.iter().filter(|token| token.chars().any(char::is_whitespace) && token.contains("--manifest-path")) {
            if nested == source {
                *unresolved = true;
            } else {
                collect_cargo_manifest_paths_from_source_at_depth(nested, case_insensitive_tools, paths, unresolved, depth + 1);
            }
        }
        let Some(tool_index) = tokens.iter().position(|token| is_manifest_capable_tool_token(token, case_insensitive_tools)) else {
            continue;
        };
        let arguments = &tokens[tool_index.saturating_add(1)..];
        let mut index = 0;
        while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
            let manifest = if argument == "--manifest-path" {
                index += 1;
                arguments.get(index).map(String::as_str)
            } else {
                argument.strip_prefix("--manifest-path=")
            };
            if let Some(manifest) = manifest {
                record_manifest_path(manifest, paths, unresolved);
            } else if argument == "--manifest-path" {
                *unresolved = true;
            }
            index += 1;
        }
    }
}

fn record_manifest_path(manifest: &str, paths: &mut BTreeSet<String>, unresolved: &mut bool) {
    if is_normalized_manifest_path(manifest) && !path::contains_dynamic_value(manifest) {
        paths.insert(manifest.to_owned());
    } else {
        *unresolved = true;
    }
}

fn is_manifest_capable_tool_token(token: &str, case_insensitive: bool) -> bool {
    is_cargo_tool_token(token, case_insensitive) || matches_tool_name(tool_basename(token), case_insensitive, &["cargo-clippy", "cargo-clippy.exe"])
}

fn collect_execution_inputs<'a>(sources: impl IntoIterator<Item = &'a str>, direct_program_paths: bool, path: &str) -> (BTreeSet<String>, bool) {
    let mut inputs = BTreeSet::new();
    let mut unresolved = false;
    let make_surface = matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase().as_str(),
        "makefile" | "gnumakefile"
    );
    let surface = ShellSurface {
        path,
        direct_program_paths,
        make_surface,
    };
    let nested_surface = ShellSurface { make_surface: false, ..surface };
    for source in sources {
        unresolved |= super::dynamic::has_opaque_command_assignment_flow(path, source);
        unresolved |= super::untrusted_directory_change_with_quality_dispatcher(source, true);
        let normalized;
        let source = if direct_program_paths {
            unresolved |= tokens::has_executable_unquoted_heredoc(source);
            normalized = tokens::without_noncommand_shell_data(source);
            let (process_commands, opaque) = tokens::process_substitution_commands(&normalized);
            unresolved |= opaque;
            for command in process_commands {
                record_shell_source_inputs(nested_surface, &command, &mut inputs, &mut unresolved);
            }
            let (command_substitutions, opaque) = tokens::command_substitution_commands(&normalized, true);
            unresolved |= opaque;
            for command in command_substitutions {
                record_shell_source_inputs(nested_surface, &command, &mut inputs, &mut unresolved);
            }
            normalized.as_str()
        } else {
            source
        };
        record_shell_source_inputs(surface, source, &mut inputs, &mut unresolved);
    }
    (inputs, unresolved)
}

#[derive(Clone, Copy)]
struct ShellSurface<'a> {
    path: &'a str,
    direct_program_paths: bool,
    make_surface: bool,
}

fn record_shell_source_inputs(surface: ShellSurface<'_>, source: &str, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    for tokens in tokens::source_command_tokens(source) {
        if surface.direct_program_paths && !surface.make_surface && substitution_is_command_program(&tokens) {
            *unresolved = true;
        }
        record_execution_inputs_from_tokens(surface.path, &tokens, surface.direct_program_paths, inputs, unresolved);
    }
}

fn substitution_is_command_program(tokens: &[String]) -> bool {
    let Some(position) = command_position(tokens) else {
        return false;
    };
    if !is_environment_assignment(position.word) && (position.word.contains("$(") || position.word.contains('`')) {
        return true;
    }
    let command = tool_basename(nested_command_token(position.word)).to_ascii_lowercase();
    matches!(
        wrapper::select(position.raw, &command, &tokens[position.index.saturating_add(1)..]),
        wrapper::Selection::Nested(nested) if substitution_is_command_program(nested)
    )
}

struct CommandPosition<'a> {
    index: usize,
    raw: &'a str,
    word: &'a str,
}

fn command_position(tokens: &[String]) -> Option<CommandPosition<'_>> {
    let index = tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_execution_input_prefix(word) && (!is_environment_assignment(word) || word.contains("$(") || word.contains('`'))
    })?;
    let raw = tokens[index].as_str();
    Some(CommandPosition {
        index,
        raw,
        word: raw.trim_matches(['(', ')', '{', '}']),
    })
}

fn record_execution_inputs_from_tokens(path: &str, tokens: &[String], direct_program_paths: bool, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    let (candidates, opaque) = execution_input_candidates(path, tokens, direct_program_paths);
    *unresolved |= opaque;
    record_execution_inputs(candidates, inputs, unresolved);
}

fn record_execution_inputs(candidates: Vec<&str>, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    for candidate in candidates {
        if let Some(input) = path::normalize_literal(candidate) {
            inputs.insert(input);
        } else {
            *unresolved = true;
        }
    }
}

fn execution_input_candidates<'a>(path: &str, tokens: &'a [String], direct_program_paths: bool) -> (Vec<&'a str>, bool) {
    let Some(position) = command_position(tokens) else {
        return (Vec::new(), false);
    };
    let command_index = position.index;
    let raw_command_word = position.raw;
    let command_word = position.word;
    if command_word.chars().any(char::is_whitespace) && (command_word.contains("$(") || command_word.contains('`')) {
        return (Vec::new(), false);
    }
    if command_word.contains("$'") {
        return (Vec::new(), true);
    }
    let command_token = nested_command_token(command_word);
    let command = tool_basename(command_token).to_ascii_lowercase();
    let arguments = &tokens[command_index.saturating_add(1)..];
    match wrapper::select(raw_command_word, &command, arguments) {
        wrapper::Selection::NotWrapper => {}
        wrapper::Selection::NoCommand => return (Vec::new(), false),
        wrapper::Selection::Nested(command) => return execution_input_candidates(path, command, direct_program_paths),
        wrapper::Selection::Opaque => return (Vec::new(), true),
    }
    if mutation::dispatch_is_opaque(path, &command, arguments) || compiler::dispatch_is_opaque(&command, arguments) || native::dispatch_is_opaque(&command, arguments) {
        return (Vec::new(), true);
    }
    let selected = match command.as_str() {
        "trap" if !command_word.contains(['/', '\\']) => return (Vec::new(), trap::action_is_opaque(path, arguments, direct_program_paths)),
        "alias" | "coproc" | "eval" | "fc" | "fc.exe" | "iex" | "invoke-expression" | "parallel" | "parallel.exe" | "trap" | "xargs" | "xargs.exe" => SelectedInput::Opaque,
        "enable" if bash_enable_loads_builtin(arguments) => SelectedInput::Opaque,
        "mapfile" | "readarray" if mapfile_callback_is_opaque(arguments) => SelectedInput::Opaque,
        "bash" | "bash.exe" | "dash" | "dash.exe" | "fish" | "fish.exe" | "sh" | "sh.exe" | "zsh" | "zsh.exe" => shell_input(&command, arguments),
        _ if dynamic_program::is_python_interpreter(&command) => python_input(arguments),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => powershell_input(arguments),
        "cmd" | "cmd.exe" | "command.com" => SelectedInput::Opaque,
        _ if dynamic_program::is_unanalyzed_interpreter(&command) => SelectedInput::Opaque,
        _ if dynamic_loader_is_opaque(&command) => SelectedInput::Opaque,
        "awk" | "awk.exe" | "dbus-run-session" | "dpkg" | "dpkg.exe" | "gawk" | "gawk.exe" | "gio" | "gio.exe" | "m4" | "m4.exe" | "mawk" | "mawk.exe" | "nawk" | "nawk.exe"
        | "protoc" | "protoc.exe" | "rake" | "rake.exe" | "rsync" | "rsync.exe" | "run-parts" | "run-parts.exe" | "sqlite3" | "sqlite3.exe" | "wget" | "wget.exe" => {
            SelectedInput::Opaque
        }
        "find" | "find.exe" => {
            return (Vec::new(), find_command_action_is_opaque(arguments));
        }
        "sed" | "sed.exe" => {
            return (Vec::new(), sed::program_is_opaque(arguments));
        }
        "sort" | "sort.exe" if sort_compression_program_is_opaque(arguments) => SelectedInput::Opaque,
        "tar" | "tar.exe" if tar_dispatch_is_opaque(arguments) => SelectedInput::Opaque,
        "zip" | "zip.exe" if zip_test_command_is_opaque(arguments) => SelectedInput::Opaque,
        "openssl" | "openssl.exe" if openssl_module_selection_is_opaque(arguments) => SelectedInput::Opaque,
        "rg" | "rg.exe" | "ripgrep" | "ripgrep.exe" if ripgrep_preprocessor_is_opaque(arguments) => SelectedInput::Opaque,
        "cargo" | "cargo.exe" if cargo::dispatch_is_opaque(arguments) => SelectedInput::Opaque,
        "go" | "go.exe" => SelectedInput::Opaque,
        "just" | "just.exe" if just_source_selection_is_opaque(arguments) => SelectedInput::Opaque,
        "make" | "make.exe" | "gmake" | "gmake.exe" => {
            let (inputs, opaque) = makefile_inputs(arguments);
            return (inputs, opaque || make_environment_selection_is_opaque(&tokens[..command_index]));
        }
        "git" | "git.exe" => {
            return (Vec::new(), git::dispatch_is_opaque(arguments));
        }
        _ => match path::select_program(command_token, direct_program_paths) {
            path::ProgramPath::NotPath => return (Vec::new(), false),
            path::ProgramPath::Literal(candidate) => SelectedInput::Literal(candidate),
            path::ProgramPath::Opaque => SelectedInput::Opaque,
        },
    };
    match selected {
        SelectedInput::None => (Vec::new(), false),
        SelectedInput::Literal(candidate) => (vec![candidate], false),
        SelectedInput::Opaque => (Vec::new(), true),
    }
}

fn dynamic_loader_is_opaque(command: &str) -> bool {
    command == "ld.so" || command.starts_with("ld.so.") || command.starts_with("ld-") && command.contains(".so")
}

fn bash_enable_loads_builtin(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == "-f" || argument.starts_with("-f") && argument.len() > 2)
}

fn tar_dispatch_is_opaque(arguments: &[String]) -> bool {
    let command_option = arguments.iter().any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option.len() >= "--checkpoint-a".len() && "--checkpoint-action".starts_with(option) || matches!(option, "-I" | "--rsh-command" | "--to-command" | "--use-compress-program")
    });
    command_option || tar_extracts_files(arguments) && !tar_extracts_into_isolated_directory(arguments)
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

fn tar_extracts_into_isolated_directory(arguments: &[String]) -> bool {
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
            if !isolated_tar_directory(destination) {
                return false;
            }
            isolated = true;
        }
        index += 1;
    }
    isolated
}

fn isolated_tar_directory(destination: &str) -> bool {
    if path::normalize_literal(destination).as_deref() == Some("extracted") {
        return true;
    }
    ["$RUNNER_TEMP/", "${RUNNER_TEMP}/"]
        .iter()
        .any(|prefix| destination.strip_prefix(prefix).is_some_and(|suffix| path::normalize_literal(suffix).is_some()))
}

fn sort_compression_program_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option.len() >= "--co".len() && "--compress-program".starts_with(option)
    })
}

fn zip_test_command_is_opaque(arguments: &[String]) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument.starts_with("-TT"))
}

fn openssl_module_selection_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        matches!(option, "-engine" | "-provider" | "-provider-path")
    })
}

fn ripgrep_preprocessor_is_opaque(arguments: &[String]) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument == "--pre" || argument.starts_with("--pre="))
}

fn just_source_selection_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        matches!(argument.as_str(), "-d" | "-f" | "-s" | "--justfile" | "--search" | "--working-directory")
            || argument.starts_with("--justfile=")
            || argument.starts_with("--working-directory=")
            || matches!(argument.as_bytes(), [b'-', b'd' | b'f', ..] if argument.len() > 2)
    })
}

fn is_execution_input_prefix(word: &str) -> bool {
    matches!(word, "!" | "if" | "then" | "elif" | "while" | "until" | "do") || word.starts_with('-')
}

fn nested_command_token(token: &str) -> &str {
    let token = token.rsplit_once("$(").map_or(token, |(_, command)| command);
    token.rsplit_once('`').map_or(token, |(_, command)| command)
}

fn supports_direct_program_paths(path: &str) -> bool {
    let path = Path::new(path);
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(basename.to_ascii_lowercase().as_str(), "justfile" | ".justfile" | "makefile" | "gnumakefile")
        || path.extension().and_then(|extension| extension.to_str()).is_none_or(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "sh" | "bash" | "zsh" | "fish" | "ps1" | "cmd" | "bat" | "just" | "toml" | "txt"
            )
        })
}

enum SelectedInput<'a> {
    None,
    Literal(&'a str),
    Opaque,
}

fn shell_input<'a>(command: &str, arguments: &'a [String]) -> SelectedInput<'a> {
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
            return arguments.get(index + 1).map_or(SelectedInput::Opaque, |candidate| SelectedInput::Literal(candidate));
        }
        if argument.starts_with(['-', '+']) {
            continue;
        }
        return SelectedInput::Literal(argument);
    }
    SelectedInput::Opaque
}

fn python_input(arguments: &[String]) -> SelectedInput<'_> {
    let mut after_options = false;
    for argument in arguments {
        if !after_options && matches!(argument.as_str(), "-h" | "--help" | "-V" | "--version") {
            return SelectedInput::None;
        }
        if !after_options && argument == "-c" {
            return SelectedInput::Opaque;
        }
        if !after_options && argument == "-m" {
            return SelectedInput::Opaque;
        }
        if !after_options && argument == "--" {
            after_options = true;
            continue;
        }
        if !after_options && argument.starts_with('-') {
            if is_python_flag_without_operand(argument) {
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

fn is_python_flag_without_operand(argument: &str) -> bool {
    matches!(
        argument,
        "-b" | "-bb" | "-B" | "-d" | "-E" | "-i" | "-I" | "-O" | "-OO" | "-P" | "-q" | "-R" | "-s" | "-S" | "-u" | "-v" | "-x"
    ) || argument.starts_with("-W") && argument.len() > 2
        || argument.starts_with("-X") && argument.len() > 2
        || argument.starts_with("--check-hash-based-pycs=")
}

fn powershell_input(arguments: &[String]) -> SelectedInput<'_> {
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
            if is_powershell_flag_without_operand(&lowercase) {
                continue;
            }
            return SelectedInput::Opaque;
        }
        return SelectedInput::Literal(argument);
    }
    SelectedInput::Opaque
}

fn is_powershell_flag_without_operand(argument: &str) -> bool {
    matches!(
        argument,
        "-login" | "-mta" | "-noexit" | "-nologo" | "-noninteractive" | "-noprofile" | "-noprofileloadtime" | "-sshservermode" | "-sta"
    )
}

fn find_command_action_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| matches!(argument.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
}

fn mapfile_callback_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").any(|argument| {
        path::contains_dynamic_value(argument)
            || argument
                .strip_prefix('-')
                .filter(|options| !options.starts_with('-'))
                .is_some_and(|options| options.contains('C'))
    })
}

fn makefile_inputs(arguments: &[String]) -> (Vec<&str>, bool) {
    let mut inputs = Vec::new();
    let mut opaque = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if make_argument_is_opaque(argument) {
            opaque = true;
            index += 1;
            continue;
        }
        let expects_operand = matches!(argument.as_str(), "-f" | "--file" | "--makefile");
        let candidate = if expects_operand {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else {
            argument
                .strip_prefix("--file=")
                .or_else(|| argument.strip_prefix("--makefile="))
                .or_else(|| argument.strip_prefix("-f").filter(|candidate| !candidate.is_empty()))
        };
        match candidate {
            Some("-") => opaque = true,
            Some(candidate) => inputs.push(candidate),
            None if expects_operand => opaque = true,
            None => {}
        }
        index += 1;
    }
    (inputs, opaque)
}

fn make_argument_is_opaque(argument: &str) -> bool {
    matches!(argument, "-C" | "-E" | "--directory" | "--eval")
        || argument.starts_with("--directory=")
        || argument.starts_with("-C") && argument.len() > 2
        || argument.starts_with("-E") && argument.len() > 2
        || argument.starts_with("--eval=")
        || is_make_environment_selection(argument)
}

fn make_environment_selection_is_opaque(tokens: &[String]) -> bool {
    tokens.iter().any(|token| is_make_environment_selection(token))
}

fn is_make_environment_selection(token: &str) -> bool {
    token
        .split_once('=')
        .map(|(name, _)| name)
        .is_some_and(|name| matches!(name, ".SHELLFLAGS" | "GNUMAKEFLAGS" | "MAKEFILES" | "MAKEFLAGS" | "MFLAGS" | "SHELL"))
}

#[cfg(test)]
mod tests {
    use super::{collect_cargo_manifest_paths, collect_execution_inputs};

    fn inputs(command: &str) -> (Vec<String>, bool) {
        let (candidates, opaque) = collect_execution_inputs(std::iter::once(command), true, "script/check.sh");
        (candidates.into_iter().collect(), opaque)
    }

    fn manifests(command: &str) -> (Vec<String>, bool) {
        let (candidates, opaque) = collect_cargo_manifest_paths(std::iter::once(command), true);
        (candidates.into_iter().collect(), opaque)
    }

    #[test]
    fn manifest_discovery_descends_only_into_relevant_command_text() {
        assert_eq!(
            manifests(r#"bash -c "cargo clippy --manifest-path tools/checker/Cargo.toml""#),
            (vec!["tools/checker/Cargo.toml".to_owned()], false)
        );
        assert_eq!(manifests(r#"write_manifest $'[package]\nname = "checker"'"#), (Vec::new(), false));
        assert_eq!(manifests(r"bash -c $'cargo clippy --manifest-path tools/checker/Cargo.toml'"), (Vec::new(), true));
    }

    #[test]
    fn execution_inputs_distinguish_files_from_opaque_programs() {
        assert_eq!(inputs("bash quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("/usr/bin/env -i bash -e ./quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("bash -lc 'cargo clippy'"), (Vec::new(), true));
        assert_eq!(inputs("HOME=quality bash -i quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("bash --login quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("dash -il quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("fish quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("zsh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs(r#"bash -c "$(cat quality/lint.txt)""#), (Vec::new(), true));
        assert_eq!(inputs(r#"command eval "$(printf '\143\141\162\147\157')""#), (Vec::new(), true));
        assert_eq!(inputs("Invoke-Expression $encoded"), (Vec::new(), true));
        assert_eq!(inputs("./quality/run-lints"), (vec!["quality/run-lints".to_owned()], false));
        assert_eq!(inputs("quality/run-lints"), (vec!["quality/run-lints".to_owned()], false));
        assert_eq!(inputs("./$PROGRAM"), (Vec::new(), true));
        assert_eq!(inputs("/tmp/lint"), (Vec::new(), true));
        assert_eq!(inputs("/usr/bin/uname -s"), (Vec::new(), false));
        assert_eq!(inputs("../quality/run-lints"), (Vec::new(), true));
        assert_eq!(inputs(r"'.\quality\run-lints.cmd'"), (Vec::new(), true));
        assert_eq!(inputs("status=$(./quality/run-lints)"), (vec!["quality/run-lints".to_owned()], false));
        assert_eq!(inputs("kernel=$(/usr/bin/uname -s)"), (Vec::new(), false));
        assert_eq!(inputs("$(printf sh) quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("`printf sh` quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs(r#""$repository_root/script/check.sh""#), (Vec::new(), false));
        assert_eq!(inputs("bash_command=$(trusted_system_command bash)"), (Vec::new(), false));
        assert_eq!(inputs("timeout 10 sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(
            inputs("timeout --signal TERM --kill-after=2s 10s sh quality/lint.txt"),
            (vec!["quality/lint.txt".to_owned()], false)
        );
        assert_eq!(inputs("timeout 3s \"$binary\""), (Vec::new(), false));
        assert_eq!(inputs("timeout --unknown 3s sh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("nice sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("nice -n 5 sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("env -u RUSTFLAGS sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("env -i HOME=/tmp sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("env | sort"), (Vec::new(), false));
        assert_eq!(inputs("sort -Vu input.txt"), (Vec::new(), false));
        assert_eq!(inputs("sort -- --compress-program=payload"), (Vec::new(), false));
        assert_eq!(inputs("nohup sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("command -p sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("command -v cargo"), (Vec::new(), false));
        assert_eq!(inputs("exec -a lint sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("builtin eval 'cargo clippy'"), (Vec::new(), true));
        assert_eq!(inputs("alias lint='sh quality/lint.txt'"), (Vec::new(), true));
        assert_eq!(inputs("builtin alias lint='sh quality/lint.txt'"), (Vec::new(), true));
        assert_eq!(inputs("script -q -e -c 'sh quality/lint.txt' /dev/null"), (Vec::new(), true));
        assert_eq!(inputs("setpriv --no-new-privs sh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("sudo -u root sh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("time -o report sh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("ionice -c 3 sh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs(r"pattern='tokio::time::(sleep|sleep_until|interval|timeout)\('"), (Vec::new(), false));
        assert_eq!(
            inputs("make -f quality/lint.rules --file=quality/common.rules"),
            (vec!["quality/common.rules".to_owned(), "quality/lint.rules".to_owned()], false)
        );
        assert_eq!(inputs("make -f $MAKEFILE"), (Vec::new(), true));
        assert_eq!(inputs("make -C quality -f lint.rules"), (vec!["lint.rules".to_owned()], true));
        assert_eq!(inputs("make -E 'all:; sh quality/lint.txt' all"), (Vec::new(), true));
        assert_eq!(inputs("make -E'all:; sh quality/lint.txt' all"), (Vec::new(), true));
        assert_eq!(inputs("make MAKEFILES=quality/lint.rules"), (Vec::new(), true));
        assert_eq!(inputs("MAKEFILES=quality/lint.rules make"), (Vec::new(), true));
        assert_eq!(inputs("make SHELL=/bin/true check"), (Vec::new(), true));
        assert_eq!(inputs("make .SHELLFLAGS=-c check"), (Vec::new(), true));
        assert_eq!(inputs("just --justfile quality/lint.data check-quality"), (Vec::new(), true));
        assert_eq!(inputs("just -fquality/lint.data check-quality"), (Vec::new(), true));
        assert_eq!(inputs("just --working-directory quality check-quality"), (Vec::new(), true));
        assert_eq!(inputs("just check-quality"), (Vec::new(), false));
        assert_eq!(inputs("rsync -e 'sh quality/lint.txt' localhost:/missing ."), (Vec::new(), true));
        assert_eq!(inputs("runner=sh; \"$runner\" quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("command=$(cat quality/lint.txt); $command"), (Vec::new(), true));
        assert_eq!(inputs("history -s 'sh quality/lint.txt'; fc -s sh"), (Vec::new(), true));
        assert_eq!(inputs("$'\\x73\\x68' quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("printf '%s' \"$'\\x73\\x68' quality/lint.txt\""), (Vec::new(), false));
    }

    #[test]
    fn indirect_command_execution_fails_closed() {
        for command in [
            "awk -f quality/lint.awk /etc/hosts",
            "gawk --file=quality/lint.awk /etc/hosts",
            "gawk --exec quality/lint.awk /etc/hosts",
            "gawk --fil=quality/lint.awk /etc/hosts",
            "gawk -W exec=quality/lint.awk /etc/hosts",
            "awk -f $SCRIPT /etc/hosts",
            "awk 'BEGIN { system(\"sh quality/lint.txt\") }'",
            r"find /tmp -maxdepth 0 -exec sh quality/lint.txt \;",
            "xargs -a quality/args.txt sh",
            "parallel sh :::: quality/args.txt",
            "tar --checkpoint=1 --checkpoint-action=exec='sh quality/lint.txt' -cf archive.tar .",
            "tar --checkpoint-a=exec='sh quality/lint.txt' -cf archive.tar .",
            "tar --checkpoint-action exec='sh quality/lint.txt' -cf archive.tar .",
            "sort --compress-program=quality/lint.txt input.txt",
            "sort --co quality/lint.txt input.txt",
            "zip -q -T -TT 'sh quality/lint.txt' archive.zip input.txt",
            "zip -T -TTquality/lint.txt archive.zip input.txt",
            "openssl list -provider-path quality -provider lint",
            "openssl.exe req -engine lint -new",
            "cargo run --manifest-path quality/helper/Cargo.toml",
            "cargo.exe --locked run --manifest-path quality/helper/Cargo.toml",
            "cargo +1.97.0 r --manifest-path quality/helper/Cargo.toml",
            "ld.so quality/lint",
            "ld.so.1 quality/lint",
            "/lib64/ld-linux-x86-64.so.2 quality/lint",
            "/lib/ld-musl-x86_64.so.1 quality/lint",
            "go run quality/lint.go",
            "go.exe -C quality run lint.go",
            "go -C=quality run lint.go",
            "sed -nf quality/lint.sed /etc/hosts",
            "sed --file=quality/lint.sed /etc/hosts",
            "sed -f $SCRIPT /etc/hosts",
            "sed -n -e '1e sh quality/lint.txt' /etc/hosts",
            "sed --exp='1e sh quality/lint.txt' /etc/hosts",
            "sed 's/.*/sh quality\\/lint.txt/e' /etc/hosts",
            "sqlite3 :memory: '.shell sh quality/lint.txt'",
            "dbus-run-session -- sh quality/lint.txt",
            "gio launch quality/lint.desktop",
            "m4 quality/lint.m4",
            "m4.exe quality/lint.m4",
            "dpkg --pre-invoke='sh quality/lint.txt' --unpack quality/missing.deb",
            "wget --use-askpass=/tmp/askpass https://example.invalid/archive",
            "yarn exec \"sh quality/lint.txt\"",
            "yarn.cmd exec \"sh quality/lint.txt\"",
            "protoc --plugin=protoc-gen-x=quality/lint.txt --x_out=target quality/input.proto",
            "protoc.exe --plugin=protoc-gen-x=quality/lint.txt --x_out=target quality/input.proto",
            "rake --rakefile quality/lint.txt",
            "run-parts quality/hooks",
            "gcc -wrapper sh,quality/lint.txt -c quality/input.c",
            "clang -fplugin=quality/lint.so -c quality/input.c",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
        for command in [
            "find /tmp -maxdepth 0 -print",
            "tar --checkpoint=1 -cf archive.tar .",
            "tar -cf archive.tar .",
            "tar --zstd -xf dist/archive.tar.zst -C extracted",
            "tar --zstd -xf dist/archive.tar.zst -C \"$RUNNER_TEMP/archive-extracted\"",
            "openssl version",
            "openssl dgst quality/input.txt",
            "cargo metadata --manifest-path quality/helper/Cargo.toml",
            "cargo test --manifest-path Cargo.toml",
            "cargo test --manifest-path tools/maintainability/Cargo.toml",
            "cargo run --manifest-path tools/maintainability/Cargo.toml --locked -- check",
            "cargo run --manifest-path=tools/dependency-unsafe/Cargo.toml --locked -- check",
            "ld --version",
            "sed -n -e '1p' /etc/hosts",
            "sed 's/../\\\\x&/g' /etc/hosts",
        ] {
            assert_eq!(inputs(command), (Vec::new(), false), "{command}");
        }
    }

    #[test]
    fn cargo_script_dispatch_fails_closed() {
        for command in [
            "cargo +nightly -Zscript quality/lint.rs",
            "cargo.exe -Z script quality/lint.rs",
            "cargo +nightly -Zscript -- quality/lint.rs",
            "rustup run nightly cargo -Z=script quality/lint.rs",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn mise_exec_dispatch_fails_closed_unless_the_nested_command_is_explicit() {
        assert_eq!(inputs("mise x -- sh quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("mise exec -- cargo fetch"), (Vec::new(), false));
        for command in ["mise x -c 'sh quality/lint.txt'", "mise x -C quality -- sh lint.txt", "mise --quiet x -- true"] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn python_package_installers_fail_closed() {
        for command in [
            "pip install quality/helper",
            "pip3.12.exe install quality/helper.whl",
            "/usr/bin/pip3 install quality/helper",
            "env pip3.13 install quality/helper",
            "py -m pip install quality/helper",
            "pythonw.exe -m pip install quality/helper",
            "pypy3 -m pip install quality/helper",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn python_documentation_launchers_fail_closed() {
        for command in [
            "pydoc quality/lint.py",
            "pydoc.exe quality/lint.py",
            "pydoc3 quality/lint.py",
            "/usr/bin/pydoc3.12 quality/lint.py",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn go_dispatch_fails_closed() {
        for command in [
            "go version",
            "go env GOROOT",
            "go test ./quality/helper",
            "go.exe -C quality test ./helper",
            "go build -toolexec=quality/lint ./...",
            "go tool quality/lint",
            "go vet -vettool=quality/lint ./...",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn compiler_dispatch_wrappers_fail_closed() {
        for command in [
            "CCACHE_DISABLE=1 ccache sh quality/lint.txt",
            "sccache sh quality/lint.txt",
            "distcc sh quality/lint.txt",
            "gomacc sh quality/lint.txt",
            "pump sh quality/lint.txt",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn archive_and_language_dispatch_fails_closed() {
        for command in [
            "tar -xf payload.tar --transform='s|quality/lint.data|Justfile|'",
            "tar xf payload.tar",
            "tar --extract --file=payload.tar",
            "tar --get --file=payload.tar",
            "tar --extr --file=payload.tar",
            "tar --ge --file=payload.tar",
            "tar -xf payload.tar -C extracted --transform='s|payload|../Justfile|'",
            "tar -xf payload.tar -C extracted --to-command='sh quality/lint.txt'",
            "tar -I quality/lint.txt -xf payload.tar -C extracted",
            "go generate ./quality/helper",
            "go.exe -C quality generate ./helper",
            "go -C=quality generate ./helper",
            "cmd.exe /d /s /c \"powershell.exe -NoProfile -EncodedCommand payload\"",
            "cmd /c echo accepted",
            "command.com /c echo accepted",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn ripgrep_preprocessors_fail_closed() {
        for command in ["rg --pre='sh quality/lint.txt' pattern .", "ripgrep --pre quality/lint.txt pattern ."] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
        assert_eq!(inputs("rg pattern ."), (Vec::new(), false));
    }

    #[test]
    fn standalone_cargo_execution_fails_closed() {
        for command in [
            "cargo test --manifest-path quality/helper/Cargo.toml",
            "cargo build --manifest-path=quality/helper/Cargo.toml",
            "cargo nextest run --manifest-path quality/helper/Cargo.toml",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn native_plugin_loading_fails_closed() {
        for command in [
            "ar --plugin=quality/lint.so rc quality/archive.a quality/input.o",
            "gcc-ar --plugin quality/lint.so rc quality/archive.a quality/input.o",
            "ld -plugin quality/lint.so -o quality/output quality/input.o",
            "x86_64-linux-gnu-ld @quality/link.args",
            "ssh-keygen -D quality/lint.so",
        ] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
    }

    #[test]
    fn language_and_git_dispatch_inputs_fail_closed() {
        assert_eq!(inputs(r#"tag="$(python3 script/release.py tag)""#), (vec!["script/release.py".to_owned()], false));
        assert_eq!(inputs("python3 quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("/usr/bin/python3.12 quality/lint.py"), (vec!["quality/lint.py".to_owned()], false));
        assert_eq!(inputs("python -m quality.lint"), (Vec::new(), true));
        assert_eq!(inputs("python3 -m timeit 'import os; os.system(\"sh quality/lint.txt\")'"), (Vec::new(), true));
        assert_eq!(inputs("python -m $MODULE"), (Vec::new(), true));
        assert_eq!(inputs("pwsh -File quality/lint.ps1"), (vec!["quality/lint.ps1".to_owned()], false));
        assert_eq!(inputs("perl quality/lint.pl"), (Vec::new(), true));
        assert_eq!(inputs("ruby -- quality/lint.rb"), (Vec::new(), true));
        assert_eq!(inputs("swift quality/lint.swift"), (Vec::new(), true));
        assert_eq!(inputs("tclsh quality/lint.tcl"), (Vec::new(), true));
        assert_eq!(inputs("/usr/bin/tclsh8.6 quality/lint.tcl"), (Vec::new(), true));
        assert_eq!(inputs("perl -e 'system q(cargo clippy)'"), (Vec::new(), true));
        assert_eq!(inputs("git -c alias.lint='!sh quality/lint.txt' lint"), (Vec::new(), true));
        assert_eq!(inputs("git -c core.fsmonitor='sh quality/lint.txt' status"), (Vec::new(), true));
        assert_eq!(inputs("git --exec-path=/tmp lint"), (Vec::new(), true));
        assert_eq!(inputs("git --exec-path /tmp lint"), (Vec::new(), true));
        assert_eq!(inputs("git config --global alias.lint '!sh quality/lint.txt'"), (Vec::new(), true));
        assert_eq!(inputs("git lint"), (Vec::new(), true));
        assert_eq!(inputs("git -c core.autocrlf=false status"), (Vec::new(), false));
        assert_eq!(inputs("git config --global core.autocrlf false"), (Vec::new(), false));
    }

    #[test]
    fn compact_substitutions_and_dynamic_builtins_are_opaque() {
        assert_eq!(inputs("printf '%s' \"$(/tmp/lint)\""), (Vec::new(), true));
        assert_eq!(inputs("printf '%s' 'install pinned tools with `mise install`'"), (Vec::new(), false));
        assert_eq!(inputs("enable -f /tmp/helper.so helper"), (Vec::new(), true));
    }

    #[test]
    fn mapfile_callbacks_fail_closed_without_rejecting_literal_array_reads() {
        assert_eq!(inputs("mapfile -C 'sh quality/lint.txt' -c 1 </etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("readarray -tC 'sh quality/lint.txt' -c 1 </etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("mapfile $OPTIONS lines </etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("mapfile -t lines </etc/hosts"), (Vec::new(), false));
    }

    #[test]
    fn shell_dispatch_inputs_fail_closed_without_matching_inert_text() {
        assert_eq!(inputs("cat <(sh quality/lint.txt)"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("cat <(printf ok;# )\nsh quality/lint.txt\n)"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("coproc sh quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("printf '%s' '<(sh quality/lint.txt)'"), (Vec::new(), false));
        assert_eq!(inputs("printf ok # <(sh quality/lint.txt)"), (Vec::new(), false));
    }
}
