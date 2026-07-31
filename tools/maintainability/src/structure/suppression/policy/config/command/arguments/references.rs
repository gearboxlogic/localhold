use std::collections::BTreeSet;
use std::path::Path;

use super::{
    dynamic_program, has_case_insensitive_tool_names, is_cargo_tool_token, is_environment_assignment, is_normalized_manifest_path, is_yaml, matches_tool_name,
    normalized_source_for_surface, package_json, tokens, tool_basename,
};

mod git;
mod path;
mod trap;
pub(super) mod wrapper;

pub(in crate::structure::suppression::policy::config::command) fn cargo_manifest_paths_for_surface(path: &str, source: &str) -> (BTreeSet<String>, bool) {
    let case_insensitive_tools = has_case_insensitive_tool_names(path);
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
    for tokens in tokens::normalized_shell_commands(source) {
        for nested in tokens.iter().filter(|token| token.chars().any(char::is_whitespace)) {
            collect_cargo_manifest_paths_from_source(nested, case_insensitive_tools, paths, unresolved);
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
    for source in sources {
        unresolved |= super::dynamic::has_opaque_command_assignment_flow(path, source);
        let normalized;
        let source = if direct_program_paths {
            unresolved |= tokens::has_executable_unquoted_heredoc(source);
            normalized = tokens::without_noncommand_shell_data(source);
            let (process_commands, opaque) = tokens::process_substitution_commands(&normalized);
            unresolved |= opaque;
            for command in process_commands {
                record_shell_source_inputs(&command, direct_program_paths, &mut inputs, &mut unresolved);
            }
            normalized.as_str()
        } else {
            source
        };
        record_shell_source_inputs(source, direct_program_paths, &mut inputs, &mut unresolved);
    }
    (inputs, unresolved)
}

fn record_shell_source_inputs(source: &str, direct_program_paths: bool, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    for tokens in tokens::source_command_tokens(source) {
        for nested_tokens in tokens
            .iter()
            .filter(|token| token.contains("$(") || token.contains('`'))
            .flat_map(|nested| tokens::source_command_tokens(nested))
        {
            record_execution_inputs_from_tokens(&nested_tokens, direct_program_paths, inputs, unresolved);
        }
        record_execution_inputs_from_tokens(&tokens, direct_program_paths, inputs, unresolved);
    }
}

fn record_execution_inputs_from_tokens(tokens: &[String], direct_program_paths: bool, inputs: &mut BTreeSet<String>, unresolved: &mut bool) {
    let (candidates, opaque) = execution_input_candidates(tokens, direct_program_paths);
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

fn execution_input_candidates(tokens: &[String], direct_program_paths: bool) -> (Vec<&str>, bool) {
    let command_index = tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_execution_input_prefix(word) && (!is_environment_assignment(word) || word.contains("$(") || word.contains('`'))
    });
    let Some(command_index) = command_index else {
        return (Vec::new(), false);
    };
    let raw_command_word = &tokens[command_index];
    let command_word = raw_command_word.trim_matches(['(', ')', '{', '}']);
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
        wrapper::Selection::Nested(command) => return execution_input_candidates(command, direct_program_paths),
        wrapper::Selection::Opaque => return (Vec::new(), true),
    }
    let selected = match command.as_str() {
        "trap" if !command_word.contains(['/', '\\']) => return (Vec::new(), trap::action_is_opaque(arguments, direct_program_paths)),
        "alias" | "coproc" | "eval" | "iex" | "invoke-expression" | "parallel" | "parallel.exe" | "trap" | "xargs" | "xargs.exe" => SelectedInput::Opaque,
        "enable" if bash_enable_loads_builtin(arguments) => SelectedInput::Opaque,
        "mapfile" | "readarray" if mapfile_callback_is_opaque(arguments) => SelectedInput::Opaque,
        "bash" | "bash.exe" | "dash" | "dash.exe" | "fish" | "fish.exe" | "sh" | "sh.exe" | "zsh" | "zsh.exe" => shell_input(arguments),
        "python" | "python.exe" | "python3" | "python3.exe" => python_input(arguments),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => powershell_input(arguments),
        _ if dynamic_program::is_unanalyzed_interpreter(&command) => SelectedInput::Opaque,
        "awk" | "awk.exe" | "gawk" | "gawk.exe" | "mawk" | "mawk.exe" | "nawk" | "nawk.exe" => {
            let (inputs, opaque) = program_file_inputs(arguments, &AWK_PROGRAM_FILES);
            return (Vec::new(), opaque || !inputs.is_empty());
        }
        "find" | "find.exe" => {
            return (Vec::new(), find_command_action_is_opaque(arguments));
        }
        "sed" | "sed.exe" => {
            let (inputs, opaque) = program_file_inputs(arguments, &SED_PROGRAM_FILES);
            return (Vec::new(), opaque || !inputs.is_empty());
        }
        "tar" | "tar.exe" if tar_checkpoint_action_is_opaque(arguments) => SelectedInput::Opaque,
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

fn bash_enable_loads_builtin(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == "-f" || argument.starts_with("-f") && argument.len() > 2)
}

fn tar_checkpoint_action_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| {
        let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
        option.len() >= "--checkpoint-a".len() && "--checkpoint-action".starts_with(option)
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

fn shell_input(arguments: &[String]) -> SelectedInput<'_> {
    for (index, argument) in arguments.iter().enumerate() {
        let lowercase = argument.to_ascii_lowercase();
        if matches!(lowercase.as_str(), "--help" | "--version") {
            return SelectedInput::None;
        }
        if matches!(lowercase.as_str(), "-c" | "--command" | "-s" | "--stdin")
            || (argument.starts_with('-') || argument.starts_with('+')) && !argument.starts_with("--") && argument[1..].contains(['c', 'C', 's', 'o', 'O'])
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
    for (index, argument) in arguments.iter().enumerate() {
        if !after_options && matches!(argument.as_str(), "-h" | "--help" | "-V" | "--version") {
            return SelectedInput::None;
        }
        if !after_options && argument == "-c" {
            return SelectedInput::Opaque;
        }
        if !after_options && argument == "-m" {
            return arguments
                .get(index + 1)
                .filter(|module| is_literal_python_module(module))
                .map_or(SelectedInput::Opaque, |_| SelectedInput::None);
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

fn is_literal_python_module(module: &str) -> bool {
    !module.is_empty()
        && module
            .split('.')
            .all(|segment| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
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

const AWK_PROGRAM_FILES: ProgramFileSyntax = ProgramFileSyntax::new(&["--exec", "--file"], &['E', 'f'], &['F', 'e', 'v'], &['W']);
const SED_PROGRAM_FILES: ProgramFileSyntax = ProgramFileSyntax::new(&["--file"], &['f'], &['e', 'i'], &[]);

struct ProgramFileSyntax {
    long: &'static [&'static str],
    short: &'static [char],
    short_with_operand: &'static [char],
    opaque_short: &'static [char],
}

impl ProgramFileSyntax {
    const fn new(long: &'static [&'static str], short: &'static [char], short_with_operand: &'static [char], opaque_short: &'static [char]) -> Self {
        Self {
            long,
            short,
            short_with_operand,
            opaque_short,
        }
    }
}

fn program_file_inputs<'a>(arguments: &'a [String], syntax: &ProgramFileSyntax) -> (Vec<&'a str>, bool) {
    let mut inputs = Vec::new();
    let mut opaque = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            break;
        }
        let matched_long_option = syntax.long.iter().find(|option| argument == **option);
        let selected = if matched_long_option.is_some() {
            index += 1;
            program_file_input(arguments.get(index).map(String::as_str))
        } else if let Some(candidate) = syntax
            .long
            .iter()
            .find_map(|option| argument.strip_prefix(option).and_then(|candidate| candidate.strip_prefix('=')))
        {
            program_file_input(Some(candidate))
        } else if abbreviated_long_program_file_option(argument, syntax) {
            SelectedInput::Opaque
        } else {
            let (selected, consumes_following) = short_program_file_input(argument, arguments.get(index + 1).map(String::as_str), syntax);
            if consumes_following {
                index += 1;
            }
            selected
        };
        match selected {
            SelectedInput::None => {}
            SelectedInput::Literal(candidate) => inputs.push(candidate),
            SelectedInput::Opaque => opaque = true,
        }
        index += 1;
    }
    (inputs, opaque)
}

fn abbreviated_long_program_file_option(argument: &str, syntax: &ProgramFileSyntax) -> bool {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    name.len() > 2 && name.starts_with("--") && syntax.long.iter().any(|option| option.starts_with(name))
}

fn program_file_input(candidate: Option<&str>) -> SelectedInput<'_> {
    match candidate {
        Some(candidate) if !candidate.is_empty() && candidate != "-" => SelectedInput::Literal(candidate),
        Some(_) | None => SelectedInput::Opaque,
    }
}

fn short_program_file_input<'a>(argument: &'a str, following: Option<&'a str>, syntax: &ProgramFileSyntax) -> (SelectedInput<'a>, bool) {
    let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) else {
        return (SelectedInput::None, false);
    };
    for (index, option) in options.char_indices() {
        if syntax.opaque_short.contains(&option) {
            return (SelectedInput::Opaque, false);
        }
        if syntax.short_with_operand.contains(&option) {
            return (SelectedInput::None, false);
        }
        if syntax.short.contains(&option) {
            let candidate = &options[index + option.len_utf8()..];
            return if candidate.is_empty() {
                (program_file_input(following), following.is_some())
            } else {
                (program_file_input(Some(candidate)), false)
            };
        }
    }
    (SelectedInput::None, false)
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
    matches!(argument, "-C" | "--directory" | "--eval")
        || argument.starts_with("--directory=")
        || argument.starts_with("-C") && argument.len() > 2
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
        .is_some_and(|name| matches!(name, "GNUMAKEFLAGS" | "MAKEFILES" | "MAKEFLAGS" | "MFLAGS"))
}

#[cfg(test)]
mod tests {
    use super::collect_execution_inputs;

    fn inputs(command: &str) -> (Vec<String>, bool) {
        let (candidates, opaque) = collect_execution_inputs(std::iter::once(command), true, "script/check.sh");
        (candidates.into_iter().collect(), opaque)
    }

    #[test]
    fn execution_inputs_distinguish_files_from_opaque_programs() {
        assert_eq!(inputs("bash quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("/usr/bin/env -i bash -e ./quality/lint.txt"), (vec!["quality/lint.txt".to_owned()], false));
        assert_eq!(inputs("bash -lc 'cargo clippy'"), (Vec::new(), true));
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
        assert_eq!(inputs("awk -f quality/lint.awk /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("gawk --file=quality/lint.awk /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("gawk --exec quality/lint.awk /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("gawk --fil=quality/lint.awk /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("gawk -W exec=quality/lint.awk /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("awk -f $SCRIPT /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs(r"find /tmp -maxdepth 0 -exec sh quality/lint.txt \;"), (Vec::new(), true));
        assert_eq!(inputs("find /tmp -maxdepth 0 -print"), (Vec::new(), false));
        assert_eq!(inputs("xargs -a quality/args.txt sh"), (Vec::new(), true));
        assert_eq!(inputs("parallel sh :::: quality/args.txt"), (Vec::new(), true));
        assert_eq!(
            inputs("tar --checkpoint=1 --checkpoint-action=exec='sh quality/lint.txt' -cf archive.tar ."),
            (Vec::new(), true)
        );
        assert_eq!(inputs("tar --checkpoint-a=exec='sh quality/lint.txt' -cf archive.tar ."), (Vec::new(), true));
        assert_eq!(inputs("tar --checkpoint-action exec='sh quality/lint.txt' -cf archive.tar ."), (Vec::new(), true));
        assert_eq!(inputs("tar --checkpoint=1 -cf archive.tar ."), (Vec::new(), false));
        assert_eq!(inputs("tar -cf archive.tar ."), (Vec::new(), false));
        assert_eq!(inputs("sed -nf quality/lint.sed /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("sed --file=quality/lint.sed /etc/hosts"), (Vec::new(), true));
        assert_eq!(inputs("sed -f $SCRIPT /etc/hosts"), (Vec::new(), true));
        assert_eq!(
            inputs("make -f quality/lint.rules --file=quality/common.rules"),
            (vec!["quality/common.rules".to_owned(), "quality/lint.rules".to_owned()], false)
        );
        assert_eq!(inputs("make -f $MAKEFILE"), (Vec::new(), true));
        assert_eq!(inputs("make -C quality -f lint.rules"), (vec!["lint.rules".to_owned()], true));
        assert_eq!(inputs("make MAKEFILES=quality/lint.rules"), (Vec::new(), true));
        assert_eq!(inputs("MAKEFILES=quality/lint.rules make"), (Vec::new(), true));
        assert_eq!(inputs("command=$(cat quality/lint.txt); $command"), (Vec::new(), true));
        assert_eq!(inputs("$'\\x73\\x68' quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("printf '%s' \"$'\\x73\\x68' quality/lint.txt\""), (Vec::new(), false));
    }

    #[test]
    fn language_and_git_dispatch_inputs_fail_closed() {
        assert_eq!(inputs(r#"tag="$(python3 script/release.py tag)""#), (vec!["script/release.py".to_owned()], false));
        assert_eq!(inputs("python3 quality/lint.txt"), (Vec::new(), true));
        assert_eq!(inputs("python -m quality.lint"), (Vec::new(), false));
        assert_eq!(inputs("python -m $MODULE"), (Vec::new(), true));
        assert_eq!(inputs("pwsh -File quality/lint.ps1"), (vec!["quality/lint.ps1".to_owned()], false));
        assert_eq!(inputs("perl quality/lint.pl"), (Vec::new(), true));
        assert_eq!(inputs("ruby -- quality/lint.rb"), (Vec::new(), true));
        assert_eq!(inputs("tclsh quality/lint.tcl"), (Vec::new(), true));
        assert_eq!(inputs("/usr/bin/tclsh8.6 quality/lint.tcl"), (Vec::new(), true));
        assert_eq!(inputs("perl -e 'system q(cargo clippy)'"), (Vec::new(), true));
        assert_eq!(inputs("git -c alias.lint='!sh quality/lint.txt' lint"), (Vec::new(), true));
        assert_eq!(inputs("git -c core.fsmonitor='sh quality/lint.txt' status"), (Vec::new(), true));
        assert_eq!(inputs("git -c core.autocrlf=false status"), (Vec::new(), false));
    }

    #[test]
    fn compact_substitutions_and_dynamic_builtins_are_opaque() {
        assert_eq!(inputs("printf '%s' \"$(/tmp/lint)\""), (Vec::new(), true));
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
